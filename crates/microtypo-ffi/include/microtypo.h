#ifndef MICROTYPO_H
#define MICROTYPO_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct microtypo_engine microtypo_engine;

typedef struct microtypo_error microtypo_error;

typedef struct microtypo_edit {
  size_t start;
  size_t end;
  uint16_t *units;
  size_t len;
} microtypo_edit;

typedef struct microtypo_trace {
  uint16_t *rule;
  size_t rule_len;
  uint8_t kind;
  size_t count;
  size_t *ranges;
  size_t ranges_len;
} microtypo_trace;

typedef struct microtypo_output {
  uint16_t *units;
  size_t len;
  size_t *changes;
  size_t changes_len;
  struct microtypo_edit *edits;
  size_t edits_len;
  struct microtypo_trace *trace;
  size_t trace_len;
} microtypo_output;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

struct microtypo_engine *microtypo_engine_new(const char *config_json,
                                              struct microtypo_error **error);

void microtypo_engine_free(struct microtypo_engine *engine);

bool microtypo_engine_call(struct microtypo_engine *engine,
                           const char *method,
                           const char *args_json,
                           struct microtypo_error **error);

bool microtypo_engine_process(struct microtypo_engine *engine,
                              const uint16_t *text,
                              size_t len,
                              bool with_changes,
                              struct microtypo_output *out,
                              struct microtypo_error **error);

bool microtypo_engine_process_diagnostics(struct microtypo_engine *engine,
                                          const uint16_t *text,
                                          size_t len,
                                          const char *options_json,
                                          struct microtypo_output *out,
                                          struct microtypo_error **error);

void microtypo_output_free(struct microtypo_output *out);

const char *microtypo_error_name(const struct microtypo_error *error);

const char *microtypo_error_message(const struct microtypo_error *error);

const char *microtypo_error_code(const struct microtypo_error *error);

const char *microtypo_error_details(const struct microtypo_error *error);

void microtypo_error_free(struct microtypo_error *error);

const char *microtypo_version(void);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* MICROTYPO_H */
