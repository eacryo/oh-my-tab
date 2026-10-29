//! The renderer's single shader, compiled at runtime from this source (as mygo's
//! `internal/gpu/metal/shader.metal`, pared down to what the clipboard UI draws):
//! every scene op is one instanced quad; the fragment stage computes the coverage of
//! rounded rectangles, inner borders, bitmap images and tinted glyph masks from
//! signed distances and texture samples. Colors are premultiplied; blending is
//! "over" (one, one-minus-src-alpha). Coordinates are device pixels with a
//! top-left origin — the CPU side multiplies points by the contents scale.
//!
//! Texture v runs in memory order: bitmaps arrive from CGBitmapContext, whose
//! row 0 is the *bottom* of the drawing, so the vertex stage maps the quad's top
//! edge to the instance's `uv.w` and its bottom edge to `uv.y`.

pub(crate) const SHADER_SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;

// Instance kinds (Inst.misc.x).
constant float K_FILL = 0.0;   // solid rounded-rect fill
constant float K_BORDER = 1.0; // fill + inner border ring (misc.z = width px)
constant float K_IMAGE = 2.0;  // premultiplied texture sample (misc.y = texture index)
constant float K_GLYPH = 3.0;  // alpha mask sample tinted by color2

constant int MAX_TEX = 16;

struct Inst {
    float4 rect;   // x, y, w, h in pixels
    float4 radii;  // corner radii: tl, tr, br, bl
    float4 color;  // premultiplied fill color
    float4 color2; // premultiplied border color, or the glyph tint
    float4 uv;     // texture rect x0,y0,x1,y1 normalized; y in memory order
    float4 clip;   // the innermost clip rect in pixels
    float4 misc;   // kind, texture index, border width, opacity
};

struct VSOut {
    float4 pos [[position]];
    float2 p;   // quad-space point in pixels (after AA expansion)
    float2 tex; // texture coordinate (v flipped for CG bitmap memory order)
    uint inst [[flat]];
};

vertex VSOut vs(uint vid [[vertex_id]], uint iid [[instance_id]],
                device const Inst *insts [[buffer(0)]],
                constant float4 &globals [[buffer(1)]]) {
    Inst i = insts[iid];
    float2 corner = float2(float(vid & 1u), float(vid >> 1u));
    float4 r = i.rect;
    float kind = i.misc.x;
    if (kind < 1.5f) {
        // Fills and borders need a pixel of room outside the rect for the AA edge.
        r = float4(r.xy - 1.0f, r.zw + 2.0f);
    }
    float2 p = r.xy + corner * r.zw;
    VSOut o;
    o.pos = float4(p / globals.xy * float2(2.0f, -2.0f) + float2(-1.0f, 1.0f), 0.0f, 1.0f);
    o.p = p;
    o.tex = float2(i.uv.x + corner.x * (i.uv.z - i.uv.x),
                   i.uv.y + (1.0f - corner.y) * (i.uv.w - i.uv.y));
    o.inst = iid;
    return o;
}

float sdRoundRect(float2 p, float4 rect, float4 radii) {
    float2 h = rect.zw * 0.5f;
    float2 q = p - rect.xy - h;
    float r = q.x < 0.0f ? (q.y < 0.0f ? radii.x : radii.w)
                       : (q.y < 0.0f ? radii.y : radii.z);
    float2 a = abs(q) - h + r;
    return length(max(a, float2(0.0f))) + min(max(a.x, a.y), 0.0f) - r;
}

float coverage(float d) { return saturate(0.5f - d); }

// Exact area of the pixel inside an unrounded rect (hairlines, the clip rect).
float areaCoverage(float2 p, float4 rect) {
    float2 c = saturate(min(rect.xy + rect.zw, p + 0.5f) - max(rect.xy, p - 0.5f));
    return c.x * c.y;
}

float rectCoverage(float2 p, float4 rect, float4 radii) {
    if (any(radii > 0.0f)) {
        return coverage(sdRoundRect(p, rect, radii));
    }
    return areaCoverage(p, rect);
}

fragment float4 fs(VSOut v [[stage_in]],
                   device const Inst *insts [[buffer(0)]],
                   array<texture2d<float>, MAX_TEX> texs [[texture(0)]]) {
    Inst i = insts[v.inst];
    float clipCov = areaCoverage(v.p, i.clip);
    if (clipCov <= 0.0f) {
        discard_fragment();
    }
    float kind = i.misc.x;
    float4 out_;
    if (kind < 0.5f) {
        out_ = i.color * rectCoverage(v.p, i.rect, i.radii);
    } else if (kind < 1.5f) {
        float w = max(i.misc.z, 0.0f);
        float4 inner = float4(i.rect.xy + w, i.rect.zw - 2.0f * w);
        float4 innerR = max(i.radii - w, float4(0.0f));
        float covOut = rectCoverage(v.p, i.rect, i.radii);
        float covIn = rectCoverage(v.p, inner, innerR);
        out_ = i.color * covIn + i.color2 * max(covOut - covIn, 0.0f);
    } else {
        constexpr sampler s(filter::linear, address::clamp_to_edge);
        float4 tex = texs[uint(i.misc.y)].sample(s, v.tex, level(0.0f));
        if (kind < 2.5f) {
            out_ = tex * i.misc.w;
        } else {
            out_ = i.color2 * tex.a * i.misc.w;
        }
    }
    return out_ * clipCov;
}
"#;
