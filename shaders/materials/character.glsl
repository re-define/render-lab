// Seven horizontal tiles contain signed distances at 30-degree light intervals.
float faceDistance(uint base, uint size, uint tile, vec2 uv) {
    vec2 p = clamp(uv * float(size) - 0.5, vec2(0), vec2(size - 1u));
    uvec2 lo = uvec2(p), hi = min(lo + 1u, uvec2(size - 1u));
    uint stride = 7u * size, origin = base + tile * size;
    vec2 f = fract(p);
    vec2 top = vec2(texels[origin + lo.y * stride + lo.x] & 255u,
                    texels[origin + lo.y * stride + hi.x] & 255u);
    vec2 bottom = vec2(texels[origin + hi.y * stride + lo.x] & 255u,
                       texels[origin + hi.y * stride + hi.x] & 255u);
    return mix(mix(top.x, top.y, f.x), mix(bottom.x, bottom.y, f.x), f.y) / 255.0 - 0.5;
}
vec3 shadeSurface(vec3 albedo, vec3 normal, vec2 uv, Instance obj) {
    float lit = step(obj.params.w, dot(normal, obj.params.xyz));
    // The head material also contains eye and mouth islands outside the face chart.
    if (obj.routing.w != 0u && uv.y > 0.235 && uv.y < 0.89 && uv.x > 0.025 && uv.x < 0.975) {
        float angle = uintBitsToFloat(obj.routing.z);
        if (angle < 0.0) uv.x = 1.0 - uv.x;
        float phase = clamp(abs(angle), 0.0, 1.0) * 6.0;
        uint tile = uint(phase);
        float a = faceDistance(obj.routing.y, obj.routing.w, tile, uv);
        float b = faceDistance(obj.routing.y, obj.routing.w, min(tile + 1u, 6u), uv);
        lit = smoothstep(-0.009, 0.009, mix(a, b, fract(phase)));
    }
    return albedo * mix(vec3(0.54, 0.43, 0.55), vec3(1.0), lit);
}
