// The W channels carry unmodified UVs through both mesh skinning and static BVH packing.
struct HitMaterial { vec4 diffuse; vec4 emission; uvec4 tex; uvec4 program; };
layout(binding=GI_MATERIALS,std430) readonly buffer HitMaterials { uvec4 hitWords[]; };
layout(binding=GI_TEXELS,std430) readonly buffer HitAtlas { uint hitTexels[]; };

vec3 hitTexel(uvec3 region, ivec2 coord) {
    ivec2 size = ivec2(region.yz);
    ivec2 p = (coord % size + size) % size;
    vec3 c = unpackUnorm4x8(hitTexels[region.x+uint(p.y)*region.y+uint(p.x)]).rgb;
    return mix(pow((c+0.055)/1.055,vec3(2.4)),c/12.92,lessThanEqual(c,vec3(0.04045)));
}
HitMaterial hitMaterial(uint id) {
    uint base=4u*id;
    return HitMaterial(uintBitsToFloat(hitWords[base]),uintBitsToFloat(hitWords[base+1u]),
        hitWords[base+2u],hitWords[base+3u]);
}
vec3 sampleHitTexture(uvec3 region,vec2 uv) {
    vec2 coord=fract(uv)*vec2(region.yz)-0.5;
    ivec2 p=ivec2(floor(coord));
    vec2 f=fract(coord);
    return mix(mix(hitTexel(region,p),hitTexel(region,p+ivec2(1,0)),f.x),
                 mix(hitTexel(region,p+ivec2(0,1)),hitTexel(region,p+1),f.x),f.y);
}
struct HitResponse { vec3 diffuse; vec3 emission; };
HitResponse shadeGiHit(vec2 uv,HitMaterial m) {
    uint model=floatBitsToUint(m.diffuse.w);
    if(model==1u) {
        vec3 v[16];
        for(uint j=0u;j<m.program.y;j++) {
            uvec4 op=hitWords[m.program.x+j];
            switch(op.x) {
                case 0u: v[j]=uintBitsToFloat(op.yzw); break;
                case 1u: v[j]=sampleHitTexture(op.yzw,uv); break;
                case 2u: v[j]=vec3(dot(v[op.y],vec3(0.2126,0.7152,0.0722))); break;
                case 3u: v[j]=v[op.y]*v[op.z]; break;
                case 4u: v[j]=v[op.y]+v[op.z]; break;
                default: v[j]=mix(v[op.y],v[op.z],clamp(v[op.w],0.,1.)); break;
            }
        }
        return HitResponse(clamp(m.diffuse.rgb*v[m.program.z],0.,1.),m.emission.rgb+v[m.program.w]);
    }
    vec3 tint=m.diffuse.rgb*sampleHitTexture(m.tex.xyz,uv);
    if(model==2u) return HitResponse(vec3(0),m.emission.rgb+tint);
    return HitResponse(tint,m.emission.rgb);
}
