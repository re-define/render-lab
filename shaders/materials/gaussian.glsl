uvec2 gaussianXY(uint packed) {
    return uvec2(packed & 65535u, packed >> 16);
}

bool gaussianContains(uint box, uvec2 point) {
    return all(greaterThanEqual(point, gaussianXY(texels[box]))) &&
           all(lessThan(point, gaussianXY(texels[box + 1u])));
}

#ifdef GAUSSIAN_COMPACT
const uint gaussianHeader = 12u, gaussianStride = 4u;
uvec2 tilePair(uint bounds) { return uvec2(bounds & 255u, (bounds >> 8) & 255u); }
bool containsCompact(uint bounds, uvec2 tile) {
    return all(greaterThanEqual(tile, tilePair(bounds))) && all(lessThanEqual(tile, tilePair(bounds >> 16)));
}
bool insideRecord(uint rec, uvec2 tile) { return containsCompact(texels[rec+3u], tile); }
#else
const uint gaussianHeader = 8u, gaussianStride = 10u;
bool insideRecord(uint rec, uvec2 tile) { return gaussianContains(rec+8u, tile); }
#endif

void addGaussian(uint rec, uvec2 pixel, vec3 scale, inout vec3 rgb) {
#ifdef GAUSSIAN_COMPACT
    vec2 center = unpackHalf2x16(texels[rec]) + vec2(tilePair(texels[rec+3u])*16u);
    vec2 d = center - vec2(pixel);
    vec2 roots = unpackHalf2x16(texels[rec+1u]);
    int attr = int(texels[rec+2u]);
    float corr = float(bitfieldExtract(attr, 24, 8)) / 128.;
    vec3 conic = vec3(roots.x*roots.x, corr*(roots.x*roots.y), roots.y*roots.y);
    vec3 color = scale * vec3(bitfieldExtract(attr,0,8), bitfieldExtract(attr,8,8), bitfieldExtract(attr,16,8));
#else
    vec2 d = uintBitsToFloat(uvec2(texels[rec], texels[rec+1])) - vec2(pixel);
    vec3 conic = uintBitsToFloat(uvec3(texels[rec+2], texels[rec+3], texels[rec+4]));
    vec3 color = uintBitsToFloat(uvec3(texels[rec+5], texels[rec+6], texels[rec+7]));
#endif
    float power = -0.5*(conic.x*d.x*d.x + conic.z*d.y*d.y) - conic.y*d.x*d.y;
    if (power > 0.) return;
    float weight = min(0.99, exp(power));
    if (weight >= 1./255.)
        rgb += weight * color;
}

vec3 gaussianColor(uint base, vec2 uv) {
    uvec2 extent = uvec2(texels[base+1], texels[base+2]);
    uvec2 pixel = min(uvec2(fract(uv)*vec2(extent)), extent-1u);
    uvec2 legacyTile = pixel/16u;
    uint records = base + gaussianHeader;
    vec3 scale = vec3(1.);
#ifdef GAUSSIAN_COMPACT
    scale = uintBitsToFloat(uvec3(texels[base+8], texels[base+9], texels[base+10]));
#endif
    vec3 rgb = vec3(0);
#if defined(GAUSSIAN_BVH)
    uint nodes = base + texels[base+6];
    uint end = texels[base+5];
    for (uint i = 0u; i < end;) {
#ifdef GAUSSIAN_COMPACT
        uint node = nodes + 2u*i;
        uint meta = texels[node+1u], link = meta & 0x0fffffffu, count = meta >> 28;
        bool hit = containsCompact(texels[node], legacyTile);
#else
        uint node = nodes + 4u*i;
        uint link = texels[node+2], count = texels[node+3];
        bool hit = gaussianContains(node, pixel);
#endif
        i = (!hit && count == 0u) ? link : i+1u;
        if (hit) {
            for (uint j = 0u; j < count; ++j) {
                uint rec = records + gaussianStride*(link+j);
                if (insideRecord(rec, legacyTile)) addGaussian(rec, pixel, scale, rgb);
            }
        }
    }
#elif defined(GAUSSIAN_SCAN)
    for (uint i = 0u; i < texels[base+4]; ++i) {
        uint rec = base+8u+10u*i;
        if (insideRecord(rec, legacyTile)) addGaussian(rec, pixel, scale, rgb);
    }
#else
    uint size = texels[base+3], nx = (extent.x+size-1u)/size;
    uint tile = (pixel.y/size)*nx + pixel.x/size;
    uint dir = base+texels[base+6];
    uint ids = base+texels[base+7];
    uint first, end;
#ifdef GAUSSIAN_COMPACT
    if (texels[base+5] <= 65535u) {
        first = (texels[dir+tile/2u] >> (16u*(tile&1u))) & 65535u;
        end = (texels[dir+(tile+1u)/2u] >> (16u*((tile+1u)&1u))) & 65535u;
    } else
#endif
    { first = texels[dir+tile]; end = texels[dir+tile+1u]; }
    for (uint i = first; i < end; ++i) {
        uint entry = texels[base+4]<=65536u ? ((texels[ids+i/2u] >> (16u*(i&1u))) & 65535u) : texels[ids+i];
#ifdef GAUSSIAN_COMPACT
        uint bits = texels[base+11], id = bitfieldExtract(entry, 0, int(bits));
        uint length = 1u + (entry >> bits);
#else
        uint id = entry, length = 1u;
#endif
        for (uint j = 0u; j < length; ++j) {
            uint rec = records+gaussianStride*(id+j);
            if (size <= 16u || insideRecord(rec, legacyTile)) addGaussian(rec, pixel, scale, rgb);
        }
    }
#endif
    return max(rgb, 0.);
}
