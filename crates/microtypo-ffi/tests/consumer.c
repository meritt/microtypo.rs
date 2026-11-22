#include "microtypo.h"
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    microtypo_error *error = NULL;
    microtypo_engine *engine = microtypo_engine_new("{\"presets\":false,\"input\":\"plain\",\"maxProcessingMs\":0,\"ruleGroups\":[{\"name\":\"custom\\ud800\",\"rules\":[{\"id\":\"r\\udc00\",\"pattern\":{\"source\":\"a\",\"flags\":\"g\"},\"replacement\":\"b\\ud800\"}]}]}", &error);
    assert(engine != NULL && error == NULL);
    const uint16_t input[] = {0xd83d, 0xde00, 97, 97};
    const uint16_t expected[] = {0xd83d, 0xde00, 98, 0xd800, 98, 0xd800};
    microtypo_output output = {0};
    assert(microtypo_engine_process_diagnostics(engine, input, 4, "{\"trace\":true,\"edits\":true,\"changes\":true}", &output, &error));
    assert(error == NULL && output.len == 6 && memcmp(output.units, expected, sizeof(expected)) == 0);
    assert(output.trace_len == 1 && output.trace[0].count == 2 && output.trace[0].kind == 0);
    const uint16_t name[] = {99, 117, 115, 116, 111, 109, 0xd800, 46, 114, 0xdc00};
    assert(output.trace[0].rule_len == 10 && memcmp(output.trace[0].rule, name, sizeof(name)) == 0);
    const size_t ranges[] = {2, 3, 3, 4};
    assert(output.trace[0].ranges_len == 2 && memcmp(output.trace[0].ranges, ranges, sizeof(ranges)) == 0);
    uint16_t rebuilt[16] = {0};
    size_t cursor = 0, count = 0;
    for (size_t i = 0; i < output.edits_len; i++) {
        const microtypo_edit *edit = &output.edits[i];
        assert(edit->start >= cursor && edit->end <= 4);
        memcpy(rebuilt + count, input + cursor, (edit->start - cursor) * sizeof(uint16_t));
        count += edit->start - cursor;
        memcpy(rebuilt + count, edit->units, edit->len * sizeof(uint16_t));
        count += edit->len;
        cursor = edit->end;
    }
    memcpy(rebuilt + count, input + cursor, (4 - cursor) * sizeof(uint16_t));
    count += 4 - cursor;
    assert(count == 6 && memcmp(rebuilt, expected, sizeof(expected)) == 0);
    microtypo_output_free(&output);
    assert(output.units == NULL && output.edits == NULL && output.trace == NULL);
    assert(!microtypo_engine_process_diagnostics(engine, input, 4, "{\"trace\":1}", &output, &error));
    assert(strcmp(microtypo_error_code(error), "ERR_MICROTYPO_CONFIG") == 0);
    microtypo_error_free(error);
    assert(!microtypo_engine_call(engine, "registerRuleGroup", "[{\"name\":\"custom\\ud800\",\"rules\":[{\"id\":\"broken\",\"pattern\":{\"source\":\"(\",\"flags\":\"g\"},\"replacement\":\"x\"}]}]", &error));
    microtypo_error_free(error);
    assert(microtypo_engine_process_diagnostics(engine, input, 4, "{\"trace\":true}", &output, &error));
    assert(error == NULL && output.len == 6 && memcmp(output.units, expected, sizeof(expected)) == 0);
    microtypo_output_free(&output);
    assert(microtypo_engine_process_diagnostics(engine, NULL, 0, "{\"trace\":true,\"edits\":true}", &output, &error));
    assert(output.len == 0 && output.edits_len == 0 && output.trace_len == 0);
    microtypo_output_free(&output);
    microtypo_engine_free(engine);
    puts("C consumer: UTF-16 diagnostics, ownership, errors and reuse passed");
    return 0;
}
