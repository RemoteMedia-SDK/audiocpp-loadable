// Benchmark adapter for audio.cpp's public C++ components. No upstream/ABI patch.
#pragma once
#include "engine/framework/debug/profiler.h"
#include "engine/framework/io/json.h"
#include "engine/models/qwen3_tts/assets.h"
#include "engine/models/qwen3_tts/speaker_encoder.h"
#include "engine/models/qwen3_tts/talker.h"
#include "engine/models/qwen3_tts/tokenizer_speech_decoder.h"
#include "engine/models/qwen3_tts/tokenizer_text.h"
#include <cmath>
#include <fstream>
#include <iomanip>
#include <limits>
#include <stdexcept>

namespace qwen_bench {
namespace qwen = engine::models::qwen3_tts;
namespace json = engine::io::json;
using Storage = engine::assets::TensorStorageType;
constexpr size_t graph_bytes = 32ull * 1024 * 1024;
constexpr size_t constant_bytes = 4ull * 1024 * 1024;
constexpr const char * revision = "06c13737cc39481787a03465697b58b1c954b56f";

inline void save_embedding(const std::filesystem::path & path,
                           const qwen::Qwen3SpeakerEmbedding & embedding,
                           const std::string & model_sha256) {
    if (embedding.dims <= 0 || embedding.values.size() != static_cast<size_t>(embedding.dims))
        throw std::runtime_error("Invalid extracted speaker embedding dimensions");
    std::ofstream file;
    file.exceptions(std::ios::badbit | std::ios::failbit);
    file.open(path);
    file << "{\"format\":\"audiocpp-qwen3-speaker-v1\",\"upstream_revision\":\"" << revision
         << "\",\"model_sha256\":" << json::stringify_string(model_sha256)
         << ",\"dimensions\":" << embedding.dims << ",\"values\":["
         << std::setprecision(std::numeric_limits<float>::max_digits10);
    for (size_t i = 0; i < embedding.values.size(); ++i) {
        if (!std::isfinite(embedding.values[i])) throw std::runtime_error("Nonfinite speaker embedding");
        if (i) file << ',';
        file << embedding.values[i];
    }
    file << "]}\n";
    file.close();
}

inline qwen::Qwen3SpeakerEmbedding load_embedding(const std::filesystem::path & path,
                                                 const std::string & model_sha256,
                                                 int64_t expected_dimensions) {
    if (std::filesystem::file_size(path) > 1024 * 1024)
        throw std::runtime_error("Speaker embedding file exceeds 1 MiB");
    const auto data = json::parse_file(path);
    if (data.require("format").as_string() != "audiocpp-qwen3-speaker-v1" ||
        data.require("upstream_revision").as_string() != revision ||
        data.require("model_sha256").as_string() != model_sha256)
        throw std::runtime_error("Speaker embedding format, runtime or model mismatch");
    qwen::Qwen3SpeakerEmbedding embedding;
    embedding.dims = data.require("dimensions").as_i64();
    const auto & values = data.require("values").as_array();
    if (embedding.dims != expected_dimensions || embedding.dims <= 0 ||
        values.size() != static_cast<size_t>(embedding.dims))
        throw std::runtime_error("Speaker embedding dimensions mismatch");
    for (const auto & item : values) {
        const auto value = item.as_f32();
        if (!std::isfinite(value)) throw std::runtime_error("Nonfinite saved speaker embedding");
        embedding.values.push_back(value);
    }
    return embedding;
}

// Matches Qwen3TTSSession's default CPU/perf-off settings and the existing
// benchmark's seed/token limit. Only speaker conditioning is changed. Unlike
// that session, this adapter needs neither reference audio nor either encoder.
class PrecomputedVoice {
public:
    PrecomputedVoice(std::shared_ptr<const qwen::Qwen3TTSAssets> assets,
                     const engine::core::BackendConfig & backend,
                     qwen::Qwen3SpeakerEmbedding embedding)
        : assets_(std::move(assets)), context_(backend), tokenizer_(assets_),
          talker_(assets_->config.talker), embedding_(std::move(embedding)) {
        if (assets_->config.variant != qwen::Qwen3TTSVariant::Base)
            throw std::runtime_error("Precomputed voice benchmark requires a Qwen3 Base model");
        weights_ = talker_.create_weights_runtime(assets_, backend, graph_bytes,
            constant_bytes, constant_bytes, Storage::Native, qwen::Qwen3TTSPerfMode::MixedAttention);
        step_ = talker_.create_step_runtime(weights_, assets_->config.talker.max_position_embeddings,
                                           assets_->config.max_new_tokens);
        decoder_ = std::make_unique<qwen::Qwen3SpeechTokenizerDecoderRuntime>(assets_, context_,
            graph_bytes, constant_bytes, Storage::Native, Storage::F32, qwen::Qwen3TTSPerfMode::MixedAttention);
    }

    engine::runtime::TaskResult run(const engine::runtime::TaskRequest & request) {
        const auto prefill_start = std::chrono::steady_clock::now();
        qwen::Qwen3TalkerPrefill prefill;
        prefill.input_ids = tokenizer_.encode(tokenizer_.build_assistant_prompt(request.text_input->text));
        if (prefill.input_ids.size() > static_cast<size_t>(assets_->config.talker.max_position_embeddings))
            throw std::runtime_error("Text exceeds model context");
        prefill.speaker_embedding = embedding_;
        prefill.language = "english";
        prefill.x_vector_only_mode = true;
        engine::debug::timing_log_scalar("qwen3_tts.prefill_build_ms", engine::debug::elapsed_ms(prefill_start));
        qwen::Qwen3TTSGenerationOptions generation;
        generation.seed = 42;
        generation.max_new_tokens = 256;
        const auto talker_start = std::chrono::steady_clock::now();
        const auto codes = step_->generate(prefill, generation, generation.repetition_penalty);
        engine::debug::timing_log_scalar("qwen3_tts.talker_ms", engine::debug::elapsed_ms(talker_start));
        const auto decoder_start = std::chrono::steady_clock::now();
        engine::runtime::TaskResult result;
        result.audio_output = decoder_->decode(codes.generated_codes);
        engine::debug::timing_log_scalar("qwen3_tts.speech_decoder_ms", engine::debug::elapsed_ms(decoder_start));
        return result;
    }

private:
    std::shared_ptr<const qwen::Qwen3TTSAssets> assets_;
    engine::core::ExecutionContext context_;
    qwen::Qwen3TextTokenizer tokenizer_;
    qwen::Qwen3Talker talker_;
    qwen::Qwen3SpeakerEmbedding embedding_;
    std::shared_ptr<const qwen::Qwen3TalkerWeightsRuntime> weights_;
    std::shared_ptr<qwen::Qwen3TalkerStepRuntime> step_;
    std::unique_ptr<qwen::Qwen3SpeechTokenizerDecoderRuntime> decoder_;
};
} // namespace qwen_bench
