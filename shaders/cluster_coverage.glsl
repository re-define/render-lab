uint hashPixel(uvec2 pixel, uint instance) {
    uint value = pixel.x * 0x1f123bb5u ^ pixel.y * 0x05491333u ^ (instance * 0x9e3779b9u);
    value ^= value >> 16;
    value *= 0x7feb352du;
    value ^= value >> 15;
    value *= 0x846ca68bu;
    return value ^ (value >> 16);
}
bool coversLod(uvec2 pixel,uvec2 transition) {
    vec2 interval=unpackUnorm2x16(transition.x);
    float sampleLod=(float(hashPixel(pixel,transition.y)&65535u)+0.5)/65536.;
    return sampleLod>=interval.x && sampleLod<interval.y;
}
