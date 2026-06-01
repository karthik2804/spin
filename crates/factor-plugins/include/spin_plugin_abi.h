/**
 * Spin Plugin C ABI (Binary Serialization)
 *
 * This header defines the C ABI for Spin plugins using binary serialization
 * via postcard. Plugins are shared libraries (.so, .dylib, .dll) that export
 * functions according to this interface.
 *
 * # Required Exports
 *
 * - `plugin_wit_text`: Returns the WIT definition as a null-terminated string
 * - `plugin_dispatch`: Dispatches function calls with postcard-serialized params/results
 *
 * # Optional Exports
 *
 * - `plugin_init`: Called at startup with postcard-serialized configuration
 * - `plugin_cleanup`: Called when the plugin is unloaded
 *
 * # Serialization Format
 *
 * Parameters and results are serialized using postcard (a no-std friendly
 * bincode variant). The Val type is defined as:
 *
 * ```
 * enum Val {
 *     Bool(bool),
 *     S8(i8), U8(u8), S16(i16), U16(u16),
 *     S32(i32), U32(u32), S64(i64), U64(u64),
 *     Float32(f32), Float64(f64),
 *     Char(u32), String(Vec<u8>),
 *     List(Vec<Val>),
 *     Record(Vec<(String, Val)>),
 *     Tuple(Vec<Val>),
 *     Variant(String, Option<Val>),
 *     Enum(String),
 *     Option(Option<Val>),
 *     Result(Option<Val>, Option<Val>), // Ok(left), Err(right)
 *     Flags(Vec<String>),
 * }
 * ```
 */

#ifndef SPIN_PLUGIN_ABI_H
#define SPIN_PLUGIN_ABI_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Returns the WIT definition for this plugin.
 *
 * @return Pointer to the null-terminated WIT definition string.
 */
const char* plugin_wit_text(void);

/**
 * Initializes the plugin with postcard-serialized configuration.
 *
 * @param config_bytes  Pointer to postcard-serialized config bytes.
 * @param config_len    Length of config bytes. May be 0 if no config.
 * @return 0 on success, non-zero on error.
 */
int plugin_init(const uint8_t* config_bytes, int32_t config_len);

/**
 * Cleans up plugin resources.
 */
void plugin_cleanup(void);

/**
 * Dispatches a function call to the plugin.
 *
 * The `func_name` is the fully qualified WIT function name:
 * `package:interface@version#function`
 *
 * @param func_name        Fully qualified WIT function name (null-terminated).
 * @param params_bytes     postcard-serialized params (Vec<Val>).
 * @param params_len       Length of params bytes.
 * @param results_buf      Buffer to write postcard-serialized results (Vec<Val>).
 * @param results_buf_len  Size of results buffer in bytes.
 * @param results_len      Output: actual bytes written to results_buf.
 * @param error_buf        Buffer to write error message on failure.
 * @param error_buf_len    Size of error buffer in bytes.
 * @return 0 on success, non-zero on error.
 */
int plugin_dispatch(
    const char* func_name,
    const uint8_t* params_bytes,
    int32_t params_len,
    uint8_t* results_buf,
    int32_t results_buf_len,
    int32_t* results_len,
    char* error_buf,
    int32_t error_buf_len
);

#ifdef __cplusplus
}
#endif

#endif /* SPIN_PLUGIN_ABI_H */
