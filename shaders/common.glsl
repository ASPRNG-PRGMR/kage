// KAGE rendering, shared by the OpenGL fragment shader and the Vulkan
// compute shader. This is the same arithmetic as the CPU renderers:
//
//   mode 0  Greyscale AA   coverage copied to all three channels
//   mode 1  Subpixel AA    fixed 3x3 kernel per channel      (layout::FilterKernel)
//   mode 2  OLED-aware     box footprint at each emitter     (render::perceptual)
//
// The including shader provides:
//   P                       the Params uniform block
//   float kage_lo(ivec2)    coverage at pixel resolution, clamped to the edge
//   float kage_hi(ivec2)    supersampled coverage, clamped to the edge

// ── Mode 1: fixed kernel ─────────────────────────────────────────────────────

float kage_row(vec3 h, ivec2 p, int dy) {
    return h.x * kage_lo(p + ivec2(-1, dy))
         + h.y * kage_lo(p + ivec2(0, dy))
         + h.z * kage_lo(p + ivec2(1, dy));
}

float kage_taps(vec3 h, vec3 v, ivec2 p) {
    float acc = 0.0;
    if (v.x != 0.0) { acc += v.x * kage_row(h, p, -1); }
    if (v.y != 0.0) { acc += v.y * kage_row(h, p, 0); }
    if (v.z != 0.0) { acc += v.z * kage_row(h, p, 1); }
    return acc;
}

vec3 kage_kernel(ivec2 p) {
    vec3 hr = P.taps_h[0].xyz;
    vec3 hg = P.taps_h[1].xyz;
    vec3 hb = P.taps_h[2].xyz;
    if (P.cell.z != 0) {
        // Pentile RGBG: red exists where x + y is even, blue where it is odd.
        // The absent colour is the mean of its two horizontal neighbours.
        vec3 absent = vec3(0.5, 0.0, 0.5);
        if (((p.x + p.y) & 1) == 0) { hb = absent; } else { hr = absent; }
    }
    return vec3(
        kage_taps(hr, P.taps_v[0].xyz, p),
        kage_taps(hg, P.taps_v[1].xyz, p),
        kage_taps(hb, P.taps_v[2].xyz, p));
}

// ── Mode 2: sampling at the emitters ─────────────────────────────────────────

// Mean supersampled coverage over a w x h pixel rectangle centred on (cx, cy).
// Pixel (col, row) spans col..col+1, row..row+1.
float kage_box(float cx, float cy, float w, float h) {
    float k = float(P.dims.z);
    w = max(w, 1.0 / k);
    h = max(h, 1.0 / k);
    float x0 = (cx - w * 0.5) * k;
    float x1 = (cx + w * 0.5) * k;
    float y0 = (cy - h * 0.5) * k;
    float y1 = (cy + h * 0.5) * k;
    int ix0 = int(floor(x0));
    int ix1 = int(ceil(x1));
    int iy0 = int(floor(y0));
    int iy1 = int(ceil(y1));
    float acc = 0.0;
    for (int sy = iy0; sy < iy1; sy++) {
        float wy = max(min(y1, float(sy) + 1.0) - max(y0, float(sy)), 0.0);
        float row = 0.0;
        for (int sx = ix0; sx < ix1; sx++) {
            float wx = max(min(x1, float(sx) + 1.0) - max(x0, float(sx)), 0.0);
            row += wx * kage_hi(ivec2(sx, sy));
        }
        acc += wy * row;
    }
    return acc / ((x1 - x0) * (y1 - y0));
}

// Sobel edge strength of the pixel-resolution coverage, weighted per axis.
float kage_edge(ivec2 p) {
    float a = kage_lo(p + ivec2(-1, -1));
    float b = kage_lo(p + ivec2(0, -1));
    float c = kage_lo(p + ivec2(1, -1));
    float d = kage_lo(p + ivec2(-1, 0));
    float f = kage_lo(p + ivec2(1, 0));
    float g = kage_lo(p + ivec2(-1, 1));
    float h = kage_lo(p + ivec2(0, 1));
    float i = kage_lo(p + ivec2(1, 1));
    float gx = P.edge.x * ((c + 2.0 * f + i) - (a + 2.0 * d + g)) / 4.0;
    float gy = P.edge.y * ((g + 2.0 * h + i) - (a + 2.0 * b + c)) / 4.0;
    return sqrt(gx * gx + gy * gy);
}

float kage_smoothstep(float lo, float hi, float x) {
    float t = clamp((x - lo) / (hi - lo), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// One colour of one pixel. `site` indexes P.sites; `luma` is true for
// emitters that carry luminance (green, or everything on a white-emitter
// panel); `edge` is how far colour emitters move to the luminance footprint.
float kage_site(ivec2 p, int site, bool luma, float edge) {
    vec4 s = P.sites[site];
    float cx = float(p.x) + s.x;
    float cy = float(p.y) + s.y;
    float wide = kage_box(cx, cy, P.footprint.x, P.footprint.w);
    if (luma) { return wide; }
    float own = kage_box(cx, cy, P.footprint.y, P.footprint.w);
    return own + edge * (wide - own);
}

vec3 kage_emitters(ivec2 p) {
    int base = ((p.y % P.cell.y) * P.cell.x + (p.x % P.cell.x)) * 3;
    float edge = 0.0;
    if (P.footprint.z > 0.0) {
        edge = P.footprint.z * kage_smoothstep(P.edge.z, P.edge.w, kage_edge(p));
    }
    bool achromatic = P.cell.w != 0;
    return vec3(
        kage_site(p, base, achromatic, edge),
        kage_site(p, base + 1, true, edge),
        kage_site(p, base + 2, achromatic, edge));
}

// ── Entry point ──────────────────────────────────────────────────────────────

vec4 kage_render(ivec2 p) {
    float cov = kage_lo(p);
    vec3 rgb;
    if (P.dims.w == 2) {
        rgb = kage_emitters(p);
    } else if (P.dims.w == 1) {
        rgb = kage_kernel(p);
    } else {
        rgb = vec3(cov);
    }
    return vec4(rgb, cov);
}
