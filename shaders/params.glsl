// Fields of the Params uniform block, shared by every backend.
// Mirrors `kage::gpu::ShaderParams` (std140; every member is a 16-byte vector).
ivec4 dims;       // width, height (logical pixels); fine samples per pixel; mode
ivec4 cell;       // unit cell width, height; pentile flag; achromatic flag
vec4  taps_h[3];  // fixed kernel: horizontal taps for red, green, blue
vec4  taps_v[3];  // fixed kernel: vertical taps for red, green, blue
vec4  footprint;  // luma width, chroma width, edge suppression, height (pixels)
vec4  edge;       // gradient weight x, y; smoothstep low, high
vec4  sites[12];  // [cell pixel * 3 + colour] = emitter x, y in the pixel
