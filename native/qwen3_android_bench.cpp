// Native Android counterpart to compare_tts.py. Uses audio.cpp's warmbench API.
#include "engine/framework/audio/wav_reader.h"
#include "engine/framework/audio/wav_writer.h"
#include "engine/framework/debug/trace.h"
#include "engine/framework/runtime/registry.h"
#include "engine/framework/runtime/session.h"
#include "qwen3_precomputed_voice.h"
#include <algorithm>
#include <chrono>
#include <cmath>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <string>
#include <sys/resource.h>
#include <vector>

using Clock = std::chrono::steady_clock;

static double elapsed(Clock::time_point start) {
    return std::chrono::duration<double, std::milli>(Clock::now() - start).count();
}

static std::vector<std::string> lines(const char * path) {
    std::ifstream input(path);
    if (!input) throw std::runtime_error(std::string("Cannot open ") + path);
    std::vector<std::string> result;
    for (std::string line; std::getline(input, line);) {
        if (!line.empty() && line.back() == '\r') line.pop_back();
        if (!line.empty()) result.push_back(line);
    }
    if (result.empty()) throw std::runtime_error(std::string("Empty input: ") + path);
    return result;
}

static void phase(const char * name, Clock::time_point start, int index = -1) {
    std::cout << "{\"phase\":\"" << name << "\",\"request\":" << index
              << ",\"milliseconds\":" << elapsed(start) << "}" << std::endl;
}

int main(int argc, char ** argv) {
    try {
        if (argc == 6 && std::string(argv[1]) == "--export-voice") {
            const auto started = Clock::now();
            auto assets = qwen_bench::qwen::load_qwen3_tts_assets(argv[2]);
            phase("model_load", started);
            engine::core::BackendConfig backend;
            backend.type = engine::core::BackendType::Cpu;
            backend.threads = 4;
            engine::core::ExecutionContext context(backend);
            const auto encoder_start = Clock::now();
            qwen_bench::qwen::Qwen3TTSEcapaTdnnEncoderRuntime encoder(
                assets, context, qwen_bench::graph_bytes, qwen_bench::Storage::F32);
            phase("speaker_encoder_create", encoder_start);
            const auto reference = engine::audio::read_wav_f32(std::filesystem::path(argv[3]));
            const auto encode_start = Clock::now();
            const auto embedding = encoder.encode({reference.sample_rate, reference.channels, reference.samples});
            phase("speaker_embedding_compute", encode_start);
            qwen_bench::save_embedding(argv[4], embedding, argv[5]);
            const auto reloaded = qwen_bench::load_embedding(argv[4], argv[5], assets->config.speaker_encoder.embedding_dim);
            if (embedding.values != reloaded.values) throw std::runtime_error("Embedding round-trip changed values");
            phase("precompute_total", started);
            return 0;
        }
        if (argc != 6 && argc != 7) throw std::runtime_error(
            "Usage: qwen3_android_bench MODEL REF.wav REF.txt CASES.txt OUTPUT_DIR [x-vector]\n"
            "       qwen3_android_bench MODEL VOICE.json MODEL_SHA256 CASES.txt OUTPUT_DIR precomputed\n"
            "       qwen3_android_bench --export-voice MODEL REF.wav VOICE.json MODEL_SHA256");
        const std::string mode = argc == 7 ? argv[6] : "full-reference";
        if (mode != "full-reference" && mode != "x-vector" && mode != "precomputed")
            throw std::runtime_error("Unknown voice conditioning mode");
        const std::filesystem::path output(argv[5]);
        std::filesystem::create_directories(output);
        engine::debug::configure_logging({true, (output / "engine-timing.log").string()});
        engine::runtime::AudioBuffer reference;
        std::string reference_text;
        if (mode != "precomputed") {
            const auto wav = engine::audio::read_wav_f32(std::filesystem::path(argv[2]));
            reference = {wav.sample_rate, wav.channels, wav.samples};
            for (const auto & line : lines(argv[3])) {
                if (!reference_text.empty()) reference_text += ' ';
                reference_text += line;
            }
        }
        auto cases = lines(argv[4]);
        const auto started = Clock::now();
        auto registry = engine::runtime::make_default_registry();
        engine::runtime::ModelLoadRequest load;
        load.model_path = argv[1]; load.family_hint = "qwen3_tts";
        auto model = mode == "precomputed" ? nullptr : registry.load(load);
        auto assets = mode == "precomputed" ? qwen_bench::qwen::load_qwen3_tts_assets(argv[1]) : nullptr;
        phase("model_load", started);

        engine::runtime::SessionOptions options;
        options.backend.type = engine::core::BackendType::Cpu;
        options.backend.threads = 4;
        qwen_bench::qwen::Qwen3SpeakerEmbedding embedding;
        if (assets) {
            const auto embedding_start = Clock::now();
            embedding = qwen_bench::load_embedding(argv[2], argv[3], assets->config.speaker_encoder.embedding_dim);
            phase("speaker_embedding_load", embedding_start);
        }
        const auto session_start = Clock::now();
        auto session = model ? model->create_task_session(
            {engine::runtime::VoiceTaskKind::Tts, engine::runtime::RunMode::Offline}, options) : nullptr;
        auto * offline = dynamic_cast<engine::runtime::IOfflineVoiceTaskSession *>(session.get());
        auto precomputed = assets ? std::make_unique<qwen_bench::PrecomputedVoice>(
            assets, options.backend, std::move(embedding)) : nullptr;
        if (!offline && !precomputed) throw std::runtime_error("Expected an offline Qwen3-TTS session");
        phase("session_create", session_start);

        for (size_t index = 0; index < cases.size(); ++index) {
            engine::runtime::TaskRequest request;
            request.text_input = engine::runtime::Transcript{cases[index], "english"};
            if (!precomputed) {
                request.voice = engine::runtime::VoiceCondition{};
                request.voice->speaker = engine::runtime::VoiceReference{};
                request.voice->speaker->audio = reference;
            }
            if (mode == "x-vector") request.options["x_vector_only_mode"] = "true";
            request.options["reference_text"] = reference_text;
            request.options["language"] = "english";
            request.options["seed"] = "42";
            request.options["max_tokens"] = "256";
            const auto prepare_start = Clock::now();
            if (session) session->prepare(engine::runtime::build_preparation_request(request));
            phase("prepare", prepare_start, static_cast<int>(index));
            std::cout << "{\"phase\":\"synthesis_started\",\"request\":" << index << "}" << std::endl;
            const auto run_start = Clock::now();
            auto result = precomputed ? precomputed->run(request) : offline->run(request);
            const double run_ms = elapsed(run_start);
            if (!result.audio_output) throw std::runtime_error("Missing synthesized audio");
            const auto & audio = *result.audio_output;
            if (audio.samples.empty() || audio.sample_rate <= 0 || audio.channels <= 0)
                throw std::runtime_error("Invalid synthesized audio");
            double peak = 0, squares = 0;
            for (float sample : audio.samples) {
                if (!std::isfinite(sample)) throw std::runtime_error("Nonfinite audio sample");
                peak = std::max(peak, std::abs(static_cast<double>(sample)));
                squares += static_cast<double>(sample) * sample;
            }
            if (peak == 0) throw std::runtime_error("Silent synthesized audio");
            engine::audio::write_pcm16_wav(output / ("request_" + std::to_string(index) + ".wav"),
                audio.sample_rate, audio.channels, audio.samples);
            rusage usage{};
            if (getrusage(RUSAGE_SELF, &usage) != 0) throw std::runtime_error("getrusage failed");
            const double seconds = static_cast<double>(audio.samples.size()) / audio.channels / audio.sample_rate;
            std::cout << "{\"phase\":\"synthesis\",\"request\":" << index
                      << ",\"milliseconds\":" << run_ms << ",\"audio_seconds\":" << seconds
                      << ",\"real_time_factor\":" << run_ms / 1000 / seconds
                      << ",\"sample_rate\":" << audio.sample_rate << ",\"channels\":" << audio.channels
                      << ",\"peak\":" << peak << ",\"rms\":" << std::sqrt(squares / audio.samples.size())
                      << ",\"process_peak_rss_kib\":" << usage.ru_maxrss << "}" << std::endl;
        }
        phase("total", started);
        return 0;
    } catch (const std::exception & error) {
        std::cerr << "Qwen3 Android benchmark failed: " << error.what() << std::endl;
        return 1;
    }
}
