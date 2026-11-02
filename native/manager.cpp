#define RM_AC_BUILD
#include "manager.h"
#include "engine/framework/package_manager/manager.h"
#include <cstring>
#include <mutex>
#include <thread>
#include <regex>
#if defined(_WIN32)
#define API extern "C" __declspec(dllexport)
#else
#define API extern "C" __attribute__((visibility("default")))
#endif
using engine::package_manager::PackageManager;
struct rm_ac_manager { std::shared_ptr<PackageManager> inner; };
struct rm_ac_job {
    std::shared_ptr<std::atomic_bool> cancel = std::make_shared<std::atomic_bool>(false);
    std::atomic<int> state{0};
    std::mutex mutex;
    uint64_t done = 0, total = 0;
    std::string message;
    std::thread thread;
    ~rm_ac_job() { cancel->store(true); if (thread.joinable()) thread.join(); }
};
static thread_local std::string error;
static std::string safe_error(const char *message) {
    auto s=std::regex_replace(std::string(message),std::regex("https?://[^ \\t\\r\\n]+"),"<remote>");
    return std::regex_replace(s,std::regex("hf_[A-Za-z0-9]+"),"<credential>");
}
static char *copy(const std::string &s) {
    auto p = new char[s.size()+1]; std::memcpy(p,s.c_str(),s.size()+1); return p;
}
template<typename F> static char *guard(F f) {
    try { error.clear(); return copy(f()); }
    catch(const std::exception &e) { error=e.what(); return nullptr; }
    catch(...) { error="unknown native exception"; return nullptr; }
}
API rm_ac_manager *rm_ac_create(const char *cache) {
    try { return new rm_ac_manager{std::make_shared<PackageManager>(std::filesystem::path{},std::filesystem::u8path(cache))}; }
    catch(const std::exception &e) { error=e.what(); return nullptr; }
    catch(...) { error="manager creation failed"; return nullptr; }
}
API void rm_ac_destroy(rm_ac_manager *m) { delete m; }
API const char *rm_ac_error() { return error.c_str(); }
API void rm_ac_string_free(char *s) { delete[] s; }
API char *rm_ac_inventory(rm_ac_manager *m,const char *id) { return guard([&]{ return m->inner->inventory(false,id); }); }
API char *rm_ac_resolve(rm_ac_manager *m,const char *id) {
    return guard([&]{
        auto p=m->inner->package_info(id);
        if(!p || !p->installed || !std::filesystem::is_regular_file(p->package.root / (std::string(".audiocpp-package-")+id+".json")))
            throw std::runtime_error("model package has no completed installation; import pre-existing files explicitly");
        if(p->package.files.size()==1 && p->package.files[0].extension()==".gguf") return (p->package.root/p->package.files[0]).u8string();
        return p->package.root.u8string();
    });
}
API char *rm_ac_remove(rm_ac_manager *m,const char *id) { return guard([&]{return m->inner->remove(id);}); }
API rm_ac_job *rm_ac_download(rm_ac_manager *m,const char *id) {
    try {
        auto job=std::make_unique<rm_ac_job>(); auto p=job.get(); auto manager=m->inner; std::string key(id);
        p->thread=std::thread([p,manager,key]{
            try {
                auto result=manager->install(key,false,p->cancel,[p](const auto &v){
                    std::lock_guard<std::mutex> lock(p->mutex); p->done=v.downloaded_bytes; p->total=v.total_bytes;
                });
                { std::lock_guard<std::mutex> lock(p->mutex); p->message=result; }
                p->state.store(1);
            } catch(const std::exception &e) {
                { std::lock_guard<std::mutex> lock(p->mutex); p->message=safe_error(e.what()); }
                p->state.store(p->cancel->load()?3:2);
            } catch(...) {
                { std::lock_guard<std::mutex> lock(p->mutex); p->message="Unknown download error"; }
                p->state.store(p->cancel->load()?3:2);
            }
        }); return job.release();
    } catch(...) { error="could not start download"; return nullptr; }
}
API int rm_ac_job_status(rm_ac_job *p,uint64_t *done,uint64_t *total) {
    std::lock_guard<std::mutex> lock(p->mutex); *done=p->done; *total=p->total; return p->state.load();
}
API char *rm_ac_job_message(rm_ac_job *p) { return guard([&]{std::lock_guard<std::mutex> lock(p->mutex);return p->message;}); }
API void rm_ac_job_cancel(rm_ac_job *p) { p->cancel->store(true); }
API void rm_ac_job_free(rm_ac_job *p) { delete p; }
