#pragma once
#include <stdint.h>
#if defined(_WIN32) && defined(RM_AC_BUILD)
#define RM_AC_API __declspec(dllexport)
#else
#define RM_AC_API
#endif
#ifdef __cplusplus
extern "C" {
#endif
typedef struct rm_ac_manager rm_ac_manager;
typedef struct rm_ac_job rm_ac_job;
RM_AC_API rm_ac_manager *rm_ac_create(const char *cache);
RM_AC_API void rm_ac_destroy(rm_ac_manager *manager);
RM_AC_API const char *rm_ac_error(void);
RM_AC_API char *rm_ac_inventory(rm_ac_manager *manager, const char *id);
RM_AC_API char *rm_ac_resolve(rm_ac_manager *manager, const char *id);
RM_AC_API char *rm_ac_remove(rm_ac_manager *manager, const char *id);
RM_AC_API void rm_ac_string_free(char *text);
RM_AC_API rm_ac_job *rm_ac_download(rm_ac_manager *manager, const char *id);
/* 0 running, 1 completed, 2 failed, 3 cancelled. Job owns its manager. */
RM_AC_API int rm_ac_job_status(rm_ac_job *job, uint64_t *done, uint64_t *total);
RM_AC_API char *rm_ac_job_message(rm_ac_job *job);
RM_AC_API void rm_ac_job_cancel(rm_ac_job *job);
/* Cancels and joins before returning. */
RM_AC_API void rm_ac_job_free(rm_ac_job *job);
#ifdef __cplusplus
}
#endif
