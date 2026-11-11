#version 330 core
// OpenGL backend: one fragment per logical pixel of the glyph, rendered into
// an RGBA32F texture. Row 0 of every texture is the top row of the glyph.

layout(std140) uniform Params {
//#include params.glsl
} P;

uniform sampler2D t_lo;   // R32F, width x height
uniform sampler2D t_hi;   // R32F, (width x height) * samples per pixel

float kage_lo(ivec2 p) {
    return texelFetch(t_lo, clamp(p, ivec2(0), P.dims.xy - 1), 0).r;
}

float kage_hi(ivec2 p) {
    return texelFetch(t_hi, clamp(p, ivec2(0), P.dims.xy * P.dims.z - 1), 0).r;
}

//#include common.glsl

out vec4 kage_out;

void main() {
    kage_out = kage_render(ivec2(gl_FragCoord.xy));
}
