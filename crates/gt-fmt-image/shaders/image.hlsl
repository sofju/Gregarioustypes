// Image pipeline: decode the transfer function, apply a gain map, convert primaries to
// linear Rec.709, map luminance to scRGB (1.0 = 80 nits) for the current display, and
// tone-map highlights above the display peak. Output is premultiplied FP16 scRGB.

cbuffer CB : register(b0)
{
    float4 dst;       // quad in NDC: x0, y0 (top), width, height
    float4 uvx;       // display uv -> source uv, row 0 (a, b, c)
    float4 uvy;       // row 1
    float4 m0;        // source linear RGB -> linear Rec.709, rows
    float4 m1;
    float4 m2;
    float4 tparams;   // x: transfer id, y: gamma exponent, z: luminance mode, w: LUT size
    float4 scales;    // x: relative scale, y: absolute (nits) scale, z: display peak, w: content peak
    float4 gmMin;     // gain map (log2)
    float4 gmMax;
    float4 gmGamma;
    float4 gmOffBase;
    float4 gmOffAlt;
    float4 gmParams;  // x: kind (0 none, 1 ISO, 2 Apple), y: weight / Apple headroom, z: single channel
    float4 misc;      // x: exposure multiplier
};

Texture2D<float4> src : register(t0);
Texture2D<float4> gmap : register(t1);
Texture2D<float4> lut : register(t2);
SamplerState samp : register(s0);
SamplerState lsamp : register(s1);

struct VSOut
{
    float4 pos : SV_Position;
    float2 uv : TEXCOORD0;
};

VSOut vs_main(uint id : SV_VertexID)
{
    float2 c = float2(id & 1, id >> 1);
    VSOut o;
    o.pos = float4(dst.x + c.x * dst.z, dst.y - c.y * dst.w, 0.0, 1.0);
    o.uv = float2(dot(uvx.xyz, float3(c, 1.0)), dot(uvy.xyz, float3(c, 1.0)));
    return o;
}

float3 srgb_eotf(float3 v)
{
    return v <= 0.04045 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4);
}

float3 bt709_inv(float3 v)
{
    return v < 0.081 ? v / 4.5 : pow((v + 0.099) / 1.099, 1.0 / 0.45);
}

// SMPTE ST 2084: signal -> nits / 10000
float3 pq_eotf(float3 e)
{
    const float m1 = 0.1593017578125;
    const float m2 = 78.84375;
    const float c1 = 0.8359375;
    const float c2 = 18.8515625;
    const float c3 = 18.6875;
    float3 p = pow(saturate(e), 1.0 / m2);
    return pow(max(p - c1, 0.0) / (c2 - c3 * p), 1.0 / m1);
}

// ARIB STD-B67 inverse OETF: signal -> scene linear [0, 1]
float3 hlg_inv_oetf(float3 e)
{
    const float a = 0.17883277;
    const float b = 0.28466892;
    const float c = 0.55991073;
    return e <= 0.5 ? e * e / 3.0 : (exp((e - c) / a) + b) / 12.0;
}

float3 decode(float3 v)
{
    int t = (int)tparams.x;
    if (t == 1) return srgb_eotf(max(v, 0.0));
    if (t == 2) return pow(max(v, 0.0), tparams.y);
    if (t == 3) return bt709_inv(max(v, 0.0));
    if (t == 4) return pq_eotf(v);
    if (t == 5) return hlg_inv_oetf(saturate(v));
    if (t == 6)
    {
        float n = tparams.w;
        float3 u = saturate(v) * ((n - 1.0) / n) + 0.5 / n;
        return float3(lut.SampleLevel(lsamp, float2(u.r, 0.5), 0).r,
                      lut.SampleLevel(lsamp, float2(u.g, 0.5), 0).g,
                      lut.SampleLevel(lsamp, float2(u.b, 0.5), 0).b);
    }
    return v;
}

// Hue-preserving soft knee on max(R,G,B): linear up to 70% of the display peak, then an
// extended-Reinhard curve that maps the content peak exactly to the display peak.
float3 tonemap(float3 c, float D, float C)
{
    float m = max(max(c.r, c.g), c.b);
    float k = 0.7 * D;
    if (m <= k) return c;
    float R = D - k;
    float U = max(C - k, 1e-4) / R;
    float u = (m - k) / R;
    float f = u * (1.0 + u / (U * U)) / (1.0 + u);
    float y = k + R * min(f, 1.0);
    return c * (y / m);
}

float4 ps_main(VSOut i) : SV_Target
{
    float4 s = src.Sample(samp, i.uv);
    float a = s.a;
    float3 c = decode(s.rgb);

    int kind = (int)gmParams.x;
    if (kind == 1)
    {
        float3 g = gmap.Sample(lsamp, i.uv).rgb;
        if (gmParams.z > 0.5) g = g.rrr;
        g = pow(saturate(g), 1.0 / gmGamma.xyz);
        float3 lg = lerp(gmMin.xyz, gmMax.xyz, g);
        c = (c + gmOffBase.xyz) * exp2(lg * gmParams.y) - gmOffAlt.xyz;
    }
    else if (kind == 2)
    {
        float g = srgb_eotf(gmap.Sample(lsamp, i.uv).rrr).r;
        c = c * (1.0 + (gmParams.y - 1.0) * g);
    }

    c = float3(dot(m0.xyz, c), dot(m1.xyz, c), dot(m2.xyz, c));

    int lum = (int)tparams.z;
    float3 o;
    if (lum == 1)
    {
        o = c * (10000.0 * scales.y);
    }
    else if (lum == 2)
    {
        // BT.2100 HLG OOTF at a 1000-nit nominal peak (system gamma 1.2).
        float Y = dot(max(c, 0.0), float3(0.2126, 0.7152, 0.0722));
        o = c * (1000.0 * pow(max(Y, 1e-6), 0.2) * scales.y);
    }
    else if (lum == 3)
    {
        o = c * (80.0 * scales.y);
    }
    else
    {
        o = c * scales.x;
    }
    o *= misc.x;
    if (scales.w > scales.z) o = tonemap(o, scales.z, scales.w);
    return float4(o * a, a);
}
