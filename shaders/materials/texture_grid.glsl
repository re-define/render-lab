// FGM1 layout in uint words: four-word header, 64 + 8 + 384 + 24
// words of paired FP16 parameters, then two UNORM8x4 words per 4x4 tile.
vec4 gridWeights4(uint offset) {
    return vec4(unpackHalf2x16(texels[offset]), unpackHalf2x16(texels[offset+1]));
}
vec3 decodeTextureGrid(uint base, uvec2 pixel) {
    uint tile = (pixel.y >> 2) * texels[base+3] + (pixel.x >> 2);
    uint features = base + 484 + tile * 2;
    vec4 lo = unpackUnorm4x8(texels[features]) * 2.0 - 1.0;
    vec4 hi = unpackUnorm4x8(texels[features+1]) * 2.0 - 1.0;
    vec4 hidden[4];
    for (uint g=0; g<4; ++g) {
        vec4 h = gridWeights4(base+68+g*2);
        for (uint j=0; j<4; ++j) {
            uint w = base+4+(g*4+j)*4;
            h[j] += dot(lo,gridWeights4(w)) + dot(hi,gridWeights4(w+2));
        }
        hidden[g] = max(h,0.0);
    }
    uint phase = (pixel.y & 3u)*4 + (pixel.x & 3u);
    vec3 rgb;
    for (uint c=0; c<3; ++c) {
        uint row = phase*3 + c;
        vec2 biases = unpackHalf2x16(texels[base+460+(row>>1)]);
        float v = biases[row & 1u];
        for (uint g=0; g<4; ++g)
            v += dot(hidden[g],gridWeights4(base+76+row*8+g*2));
        rgb[c] = v;
    }
    return clamp(rgb,0.0,1.0);
}

