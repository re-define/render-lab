#include "lighting_scene.glsl"
#include "restir_types.glsl"
#include "gi12_types.glsl"
#define RESTIR_CONFIG 2
#include "restir_controls.glsl"
#include "restir_lights.glsl"
#define LIGHTING_NODES 5
#define LIGHTING_TRIANGLES 6
#define SKINNED_SHADOWS
#define SCENE_HAS_DEFORMED (viewport.z!=0u)
#if defined(GI_SHADOW) || defined(SCENE_RAY_INLINE)
#define SCENE_RAY_HARDWARE
#ifdef PROXY_SHADOWS
#define TLAS_OFFSET 48
#else
#define TLAS_OFFSET 64
#endif
#define CACHE_BINDING 8
#define BATCH_BINDING 9
#define PATH_BINDING 10
#else
#define CACHE_BINDING 10
#define BATCH_BINDING 11
#define PATH_BINDING 12
#endif
#define SCENE_RAY_CLOSEST
#include "scene_ray_query.glsl"
#include "restir_visibility.glsl"
#define GI_MATERIALS 4
#define GI_TEXELS 7
#include "gi_hit.glsl"
layout(binding=0,std430) readonly buffer Visible { Surface surfaces[]; };
layout(binding=3,std430) readonly buffer Clock { uvec4 frame; };
layout(binding=CACHE_BINDING,std430) buffer World { Cell cells[]; };
layout(binding=BATCH_BINDING,std430) buffer Batch { Samples batches[]; };
layout(binding=PATH_BINDING,std430) writeonly buffer Paths { Path paths[]; };
#ifndef GI_SHADOW
layout(local_size_x=8) in;
shared uint anchorPixel;
#endif

struct Interaction { vec3 point; vec3 normal; uint material; vec3 albedo; vec3 emission; bool leaf; float skyVisibility; };
bool intersectWorld(vec3 o, vec3 d, out Interaction hit) {
    SceneRayHit h;
    if(!rayClosest(o,d,10000.,h)) return false;
    HitMaterial m=hitMaterial(h.material);
    vec3 n=h.normal;
    if(dot(n,d)>0.) {
        if(m.tex.w==0u) return false;
        n=-n;
    }
    HitResponse response=shadeGiHit(h.uv,m);
    bool leaf=floatBitsToUint(m.diffuse.w)==3u;
    float sky=receiverSky(n,leaf,h.uv.x);
    hit=Interaction(o+h.distance*d,n,h.material,response.diffuse,response.emission,leaf,sky);
    return true;
}
// At secondary vertices use the same light table, incident-light target and reservoir
// normalization as primary ReSTIR DI. Small authored light lists can be enumerated exactly.
vec3 outgoingDirect(Interaction h,uint slot,bool second,inout uint seed,inout Reservoir r) {
    if(options.y==0u || all(equal(h.albedo,vec3(0)))) return h.emission;
    Surface s=makeReceiver(vec4(h.point,1),h.normal,1u,h.leaf,h.skyVisibility);
    vec3 base=h.emission+h.albedo*canopySky.rgb*h.skyVisibility;

    uint count=min(options.y,min(options.z,8u));
    for(uint i=0u;i<count;i++) {
        uint lamp=count==options.y ? i : sampleLight(seed);
        float target=luma(incident(s,lamp));
        offer(r,lamp,target*float(options.y),1u,target,seed);
    }
    if(slot!=INVALID && options.y>count) {
        Reservoir old=second ? cells[slot].secondLamp : cells[slot].firstLamp;
        reuse(r,old,s,4u*count,seed);
    }
    if(r.target==0.) return base;
    return base+h.albedo*visibleLight(s,r.light,seed,1u,0.004)*normalization(r);
}
uint findCell(Interaction h,uint tick) {
    ivec3 p=ivec3(floor(h.point/0.16));
    uint n=packSnorm4x8(vec4(round(h.normal*4.)/4.,0));
    uint key=max(1u,hash(hash(uint(p.x))^hash(uint(p.y)+89u)^hash(uint(p.z)+197u)^hash(n)^hash(h.material+911u)));
    uint mask=uint(cells.length())-1u;
    for(uint i=0u;i<12u;i++) {
        uint slot=(hash(key)+i)&mask;
        uint previous=atomicCompSwap(cells[slot].tag.x,0u,key);
        if(previous==0u || previous==key) {
            atomicMax(cells[slot].tag.y,tick);
            return slot;
        }
    }
    return INVALID;
}
void recordDirect(uint slot,vec3 value,bool second,uint path) {
    if(slot==INVALID) return;
    uvec3 v=encodeLight(value);
    if(second) {
        atomicMin(batches[slot].representative.y,path);
        if(atomicAdd(batches[slot].second.w,1u)>=SAMPLE_CAP) return;
        atomicAdd(batches[slot].second.x,v.x); atomicAdd(batches[slot].second.y,v.y);
        atomicAdd(batches[slot].second.z,v.z);
    } else {
        atomicMin(batches[slot].representative.x,path);
        if(atomicAdd(batches[slot].first.w,1u)>=SAMPLE_CAP) return;
        atomicAdd(batches[slot].first.x,v.x); atomicAdd(batches[slot].first.y,v.y);
        atomicAdd(batches[slot].first.z,v.z);
    }
}
void main() {
#ifdef GI_SHADOW
    uvec2 tile=uvec2(gl_LaunchIDEXT.x/RAYS,gl_LaunchIDEXT.y);
    uint lane=gl_LaunchIDEXT.x%RAYS, columns=gl_LaunchSizeEXT.x/RAYS;
    uint anchorPixel;
#else
    uvec2 tile=gl_WorkGroupID.xy;
    uint lane=gl_LocalInvocationIndex, columns=gl_NumWorkGroups.x;
    if(lane==0u)
#endif
    {
        uvec2 start=tile*TILE;
        anchorPixel=INVALID;
        uint best=INVALID;
        for(uint i=0u;i<TILE*TILE;i++) {
            uvec2 q=start+uvec2(i%TILE,i/TILE);
            if(any(greaterThanEqual(q,screenSize))) continue;
            uint pixel=indexAt(q);
            ivec2 delta=ivec2(q-start)*2-ivec2(TILE-1u);
            uint distance2=uint(delta.x*delta.x+delta.y*delta.y);
            if(surfaces[pixel].id!=0u && distance2<best) { anchorPixel=pixel; best=distance2; }
        }
    }
#ifndef GI_SHADOW
    barrier();
#endif
    uint path=(tile.y*columns+tile.x)*RAYS+lane;
    Path p=Path(uvec4(INVALID,INVALID,anchorPixel,0u),vec4(0),vec4(0),emptyReservoir(),emptyReservoir(),vec4(0));
    if(anchorPixel!=INVALID) {
        Surface a=surfaces[anchorPixel];
        uint seed=hash(hash(frame.x+1171u)^hash(path+719u));
        float r=sqrt(random(seed));
        uint tileId=path/RAYS;
        uint rotationSeed=hash(tileId+hash(frame.x+2671u));
        float angle=TAU*((float(lane)+random(seed))/float(RAYS)+random(rotationSeed));
        vec3 normal=surfaceNormal(a);
        vec3 tangent=normalize(cross(abs(normal.y)<0.98 ? vec3(0,1,0) : vec3(1,0,0),normal));
        vec3 d=r*(cos(angle)*tangent+sin(angle)*cross(normal,tangent))+sqrt(1.-r*r)*normal;
        Interaction h;
        if(intersectWorld(a.point.xyz+normal*0.004,d,h)) {
            p.address.x=findCell(h,frame.x);
            p.first=vec4(outgoingDirect(h,p.address.x,false,seed,p.firstLamp),1.);
            recordDirect(p.address.x,p.first.rgb,false,path);
            // Half of first hits trace one more cosine-weighted ray. Use a separate RNG
            // stream so the first-bounce samples match between both comparison modes.
            if(controls.w>1.5 && any(greaterThan(h.albedo,vec3(0))) && random(seed)<0.5) {
                p.weight=vec4(h.albedo*2.,0);
                vec3 next=cosineDirection(h.normal,seed);
                Interaction h2;
                if(intersectWorld(h.point+h.normal*0.004,next,h2)) {
                    p.address.y=findCell(h2,frame.x);
                    p.second=vec4(outgoingDirect(h2,p.address.y,true,seed,p.secondLamp),1.);
                    recordDirect(p.address.y,p.second.rgb,true,path);
                }
            }
        }
    }
    paths[path]=p;
}
