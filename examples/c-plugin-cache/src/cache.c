/**
 * Example Spin Plugin: Simple In-Memory Cache
 *
 * This plugin demonstrates the Spin plugin C ABI by implementing
 * a simple in-memory key-value cache.
 *
 * Build with:
 *   gcc -shared -fPIC -o libcache.so src/cache.c -I../../crates/factor-plugins/include
 *
 * Usage:
 *   spin up --plugin cache=./libcache.so --plugin-config cache='{"max_entries": 1000}'
 */

#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include "spin_plugin_abi.h"

/* Simple JSON parsing helpers (minimal implementation) */
static int json_get_string(const char* json, const char* key, char* out, int out_len) {
    char search[256];
    snprintf(search, sizeof(search), "\"%s\":", key);

    const char* pos = strstr(json, search);
    if (!pos) return -1;

    pos = strchr(pos + strlen(search), '"');
    if (!pos) return -1;
    pos++;

    const char* end = strchr(pos, '"');
    if (!end) return -1;

    int len = (int)(end - pos);
    if (len >= out_len) len = out_len - 1;

    memcpy(out, pos, len);
    out[len] = '\0';
    return 0;
}

/* Cache entry */
typedef struct {
    char key[256];
    char value[4096];
} CacheEntry;

/* Cache state */
static CacheEntry cache_entries[10000];
static int cache_count = 0;
static int max_entries = 10000;
static int initialized = 0;

/* WIT definition for the cache plugin */
static const char* cache_wit_text =
    "package example:cache@0.1.1;\n"
    "\n"
    "interface store {\n"
    "    get: func(key: string) -> result<option<string>, string>;\n"
    "    set: func(key: string, value: string) -> result<_, string>;\n"
    "    delete: func(key: string) -> result<_, string>;\n"
    "    exists: func(key: string) -> result<bool, string>;\n"
    "}\n"
    "\n"
    "world cache-plugin {\n"
    "    export store;\n"
    "}\n";

/* Find cache entry by key */
static int find_entry(const char* key) {
    for (int i = 0; i < cache_count; i++) {
        if (strcmp(cache_entries[i].key, key) == 0) {
            return i;
        }
    }
    return -1;
}

const char* plugin_wit_text(void) {
    return cache_wit_text;
}

int plugin_init(const uint8_t* config_bytes, int32_t config_len) {
    if (config_bytes && config_len > 0) {
        char config_json[1024];
        int len = (config_len < (int32_t)sizeof(config_json) - 1) ? config_len : (int32_t)sizeof(config_json) - 1;
        memcpy(config_json, config_bytes, len);
        config_json[len] = '\0';

        char buf[64];
        if (json_get_string(config_json, "max_entries", buf, sizeof(buf)) == 0) {
            int val = atoi(buf);
            if (val > 0 && val < 100000) {
                max_entries = val;
            }
        }
    }
    initialized = 1;
    return 0;
}

void plugin_cleanup(void) {
    cache_count = 0;
    initialized = 0;
}

/*
 * Helper to write postcard-serialized result.
 *
 * For a result type like `result<option<string>, string>`, the WIT component model
 * returns the result as a single value wrapped in a Result variant.
 *
 * Postcard serialization of PluginVal uses variant discriminant as first byte.
 * PluginVal variants (0-indexed):
 *   0:Bool, 1:S8, 2:U8, 3:S16, 4:U16, 5:S32, 6:U32, 7:S64, 8:U64,
 *   9:Float32, 10:Float64, 11:Char, 12:String, 13:List, 14:Record,
 *   15:Tuple, 16:Variant, 17:Enum, 18:Option, 19:Result, 20:Flags, 21:Unit
 *
 * For Vec<PluginVal>, there's a length prefix (u128 le128 encoding).
 *
 * Result(ok, err) serializes as:
 *   - Discriminant: 19 (0x13)
 *   - ok field (Option): tag 0=Some, tag 1=None (but postcard uses 1=Some/0=None pattern)
 *   - err field (Option): same pattern
 *
 * Actually, let's just build the bytes manually based on the observed format:
 * - Result(Ok(value), None): [19, 1, <value_bytes>]
 * - Result(None, Err(error)): [19, 0, 1, <error_bytes>]
 * - Result(Ok(Unit), None): [19, 1, 21, 0]  (21=Unit, 0=empty for Option None)
 */

/* Helper: write postcard Vec<PluginVal> with single result to buffer */
static int write_postcard_result(
    uint8_t* buf, int buf_len,
    int is_ok,
    const char* ok_string,    // NULL for unit/none, non-NULL for string value
    int is_option_none,       // 1 if result is Option(None)
    const char* err_string    // non-NULL for error string
) {
    int pos = 0;

    // Length prefix for Vec: 1 (single element)
    if (pos >= buf_len) return -1;
    buf[pos++] = 1;

    // Result discriminant: 19
    if (pos >= buf_len) return -1;
    buf[pos++] = 19;

    if (is_ok) {
        // Ok field: Option (1=Some, 0=None)
        if (is_option_none) {
            // Option(None)
            if (pos >= buf_len) return -1;
            buf[pos++] = 0;
        } else if (ok_string) {
            // Option(Some(String))
            // Option variant: 1=Some
            if (pos + 1 >= buf_len) return -1;
            buf[pos++] = 1;
            // String discriminant: 12
            buf[pos++] = 12;
            // String length (u128 le128)
            int slen = (int)strlen(ok_string);
            if (pos >= buf_len) return -1;
            buf[pos++] = (uint8_t)slen;
            // String bytes
            if (pos + slen > buf_len) return -1;
            memcpy(buf + pos, ok_string, slen);
            pos += slen;
        } else {
            // Option(Some(Unit))
            // Option variant: 1=Some
            if (pos >= buf_len) return -1;
            buf[pos++] = 1;
            // Unit discriminant: 21
            if (pos >= buf_len) return -1;
            buf[pos++] = 21;
        }
        // Err field: Option(None) = 0
        if (pos >= buf_len) return -1;
        buf[pos++] = 0;
    } else {
        // Ok field: Option(None) = 0
        if (pos >= buf_len) return -1;
        buf[pos++] = 0;
        // Err field: Option(Some(String))
        // Option variant: 1=Some
        if (pos >= buf_len) return -1;
        buf[pos++] = 1;
        // String discriminant: 12
        if (pos >= buf_len) return -1;
        buf[pos++] = 12;
        // String length
        int slen = err_string ? (int)strlen(err_string) : 0;
        if (pos >= buf_len) return -1;
        buf[pos++] = (uint8_t)slen;
        // String bytes
        if (err_string && pos + slen > buf_len) return -1;
        if (err_string) {
            memcpy(buf + pos, err_string, slen);
            pos += slen;
        }
    }

    return pos;
}

/* Helper: extract string from postcard-serialized params */
static int extract_param_string(const uint8_t* params, int params_len, int index, char* out, int out_len) {
    // This is a simplified parser for postcard Vec<PluginVal::String>
    // For a single string param: [len_prefix(1), discriminant(12), str_len, str_bytes...]
    if (params_len < 3) return -1;

    // Skip length prefix (first byte, should be number of params)
    int pos = 0;
    uint8_t num_params = params[pos++];

    // Skip to the requested index
    for (int i = 0; i < index && i < num_params; i++) {
        // Read discriminant
        if (pos >= params_len) return -1;
        uint8_t disc = params[pos++];

        // If String (discriminant 12), skip string
        if (disc == 12) {
            if (pos >= params_len) return -1;
            uint8_t slen = params[pos++];
            pos += slen;
        } else {
            // For other types, we'd need full postcard parsing
            // For simplicity, return error
            return -1;
        }
    }

    // Now at the requested param
    if (pos >= params_len) return -1;
    uint8_t disc = params[pos++];
    if (disc != 12) return -1; // Not a string

    if (pos >= params_len) return -1;
    uint8_t slen = params[pos++];
    if (slen >= out_len) slen = out_len - 1;
    if (pos + slen > params_len) return -1;

    memcpy(out, params + pos, slen);
    out[slen] = '\0';
    return 0;
}

int plugin_dispatch(
    const char* func_name,
    const uint8_t* params_bytes,
    int32_t params_len,
    uint8_t* results_buf,
    int32_t results_buf_len,
    int32_t* results_len,
    char* error_buf,
    int32_t error_buf_len)
{
    /* Parse function name: package:interface@version#function */
    const char* hash = strrchr(func_name, '#');
    if (!hash) {
        snprintf(error_buf, error_buf_len, "invalid function name: %s", func_name);
        return 1;
    }

    const char* func = hash + 1;

    /* Handle cache.get(key) -> result<option<string>, string> */
    if (strcmp(func, "get") == 0) {
        char key[256];
        if (extract_param_string(params_bytes, params_len, 0, key, sizeof(key)) != 0) {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                0, /* is_ok */
                NULL, 0, /* ok params */
                "missing key parameter" /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
            return 0;
        }

        if (!initialized) {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                0, /* is_ok */
                NULL, 0, /* ok params */
                "cache not initialized" /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
            return 0;
        }

        int idx = find_entry(key);
        if (idx >= 0) {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                1, /* is_ok */
                cache_entries[idx].value, /* ok_string */
                0, /* is_option_none */
                NULL /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
        } else {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                1, /* is_ok */
                NULL, /* ok_string */
                1, /* is_option_none */
                NULL /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
        }
        return 0;
    }

    /* Handle cache.set(key, value) -> result<_, string> */
    if (strcmp(func, "set") == 0) {
        char key[256];
        char value[4096];

        if (extract_param_string(params_bytes, params_len, 0, key, sizeof(key)) != 0) {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                0, /* is_ok */
                NULL, 0, /* ok params */
                "missing key parameter" /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
            return 0;
        }

        if (extract_param_string(params_bytes, params_len, 1, value, sizeof(value)) != 0) {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                0, /* is_ok */
                NULL, 0, /* ok params */
                "missing value parameter" /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
            return 0;
        }

        if (!initialized) {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                0, /* is_ok */
                NULL, 0, /* ok params */
                "cache not initialized" /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
            return 0;
        }

        /* Update existing or add new */
        int idx = find_entry(key);
        if (idx >= 0) {
            strncpy(cache_entries[idx].value, value, sizeof(cache_entries[idx].value) - 1);
        } else if (cache_count < max_entries) {
            strncpy(cache_entries[cache_count].key, key, sizeof(cache_entries[cache_count].key) - 1);
            strncpy(cache_entries[cache_count].value, value, sizeof(cache_entries[cache_count].value) - 1);
            cache_count++;
        } else {
            char err_msg[256];
            snprintf(err_msg, sizeof(err_msg), "cache full (max: %d entries)", max_entries);
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                0, /* is_ok */
                NULL, 0, /* ok params */
                err_msg /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
            return 0;
        }

        // Result(Ok(Unit), None)
        int written = write_postcard_result(
            results_buf, (int)results_buf_len,
            1, /* is_ok */
            NULL, /* ok_string - NULL means Unit */
            0, /* is_option_none */
            NULL /* err */
        );
        if (written < 0) {
            snprintf(error_buf, error_buf_len, "results buffer too small");
            return 1;
        }
        *results_len = written;
        return 0;
    }

    /* Handle cache.delete(key) -> result<_, string> */
    if (strcmp(func, "delete") == 0) {
        char key[256];
        if (extract_param_string(params_bytes, params_len, 0, key, sizeof(key)) != 0) {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                0, /* is_ok */
                NULL, 0, /* ok params */
                "missing key parameter" /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
            return 0;
        }

        if (!initialized) {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                0, /* is_ok */
                NULL, 0, /* ok params */
                "cache not initialized" /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
            return 0;
        }

        int idx = find_entry(key);
        if (idx >= 0) {
            /* Shift entries to fill the gap */
            for (int i = idx; i < cache_count - 1; i++) {
                cache_entries[i] = cache_entries[i + 1];
            }
            cache_count--;
        }

        int written = write_postcard_result(
            results_buf, (int)results_buf_len,
            1, /* is_ok */
            NULL, /* ok_string - NULL means Unit */
            0, /* is_option_none */
            NULL /* err */
        );
        if (written < 0) {
            snprintf(error_buf, error_buf_len, "results buffer too small");
            return 1;
        }
        *results_len = written;
        return 0;
    }

    /* Handle cache.exists(key) -> result<bool, string> */
    if (strcmp(func, "exists") == 0) {
        char key[256];
        if (extract_param_string(params_bytes, params_len, 0, key, sizeof(key)) != 0) {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                0, /* is_ok */
                NULL, 0, /* ok params */
                "missing key parameter" /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
            return 0;
        }

        if (!initialized) {
            int written = write_postcard_result(
                results_buf, (int)results_buf_len,
                0, /* is_ok */
                NULL, 0, /* ok params */
                "cache not initialized" /* err */
            );
            if (written < 0) {
                snprintf(error_buf, error_buf_len, "results buffer too small");
                return 1;
            }
            *results_len = written;
            return 0;
        }

        int idx = find_entry(key);
        // For bool results, we need a different approach since write_postcard_result
        // doesn't handle bool. Let's write the bytes manually.
        // Result(Ok(Bool(val)), None) = [1, 19, 1, <Option(Some(Bool)))>, 0]
        // Option(Some(Bool(val))) = [1, 0, val]  (1=Some, 0=Bool discriminant, val=0/1)
        if (results_buf_len < 6) {
            snprintf(error_buf, error_buf_len, "results buffer too small");
            return 1;
        }
        results_buf[0] = 1;   // Vec length: 1
        results_buf[1] = 19;  // Result discriminant
        results_buf[2] = 1;   // Ok field: Option(Some(...))
        results_buf[3] = 0;   // Bool discriminant
        results_buf[4] = (idx >= 0) ? 1 : 0;  // Bool value
        results_buf[5] = 0;   // Err field: Option(None)
        *results_len = 6;
        return 0;
    }

    char err_msg[256];
    snprintf(err_msg, sizeof(err_msg), "unknown function: %s", func);
    snprintf(error_buf, error_buf_len, "%s", err_msg);
    return 1;
}
