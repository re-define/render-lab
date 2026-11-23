layout(set=0,binding=LIGHTING_NODES,std430) readonly buffer Nodes { RayNode nodes[]; };
layout(set=0,binding=LIGHTING_TRIANGLES,std430) readonly buffer Triangles { Triangle triangles[]; };
#ifdef SKINNED_SHADOWS
layout(set=0,binding=8,std430) readonly buffer SkinnedNodes { RayNode skinnedNodes[]; };
layout(set=0,binding=9,std430) readonly buffer SkinnedTriangles { Triangle skinnedTriangles[]; };
#endif

struct Hit { float distance; uint id; vec2 uv; };
bool intersectsBox(vec3 origin, vec3 inverseDir, RayNode node, float limit) {
    // The complete refit tree can have a padded suffix. Its inverted empty boxes
    // would otherwise become unbounded after the per-axis min/max below.
    if(node.low.x > node.high.x) return false;
    vec3 a = (node.low - origin) * inverseDir, b = (node.high - origin) * inverseDir;
    vec3 lo = min(a,b), hi = max(a,b);
    return max(max(max(lo.x,lo.y),lo.z),0.0) <= min(min(min(hi.x,hi.y),hi.z),limit);
}

bool intersectsTriangle(vec3 origin, vec3 dir, uint id, Triangle t, inout Hit hit) {
    vec3 e1 = t.point[1].xyz - t.point[0].xyz, e2 = t.point[2].xyz - t.point[0].xyz;
    vec3 p = cross(dir,e2);
    float det = dot(e1,p);
    if(abs(det)<1e-8) return false;
    vec3 s = origin - t.point[0].xyz;
    float u = dot(s,p) / det;
    if(u<0.0 || u>1.0) return false;
    vec3 q = cross(s,e1);
    float v = dot(dir,q) / det;
    float d = dot(e2,q) / det;
    if(v<0.0 || u+v>1.0 || d<=0.0001 || d>=hit.distance) return false;
    hit = Hit(d,id,vec2(u,v));
    return true;
}

Hit traceTree(vec3 origin, vec3 dir, float limit, bool anyHit, bool skinned) {
    vec3 inverseDir = mix(vec3(-1),vec3(1),greaterThanEqual(dir,vec3(0))) / max(abs(dir), 1e-9);
    Hit hit = Hit(limit,0xffffffffu,vec2(0));
    uint i = 0;
    uint count = nodes.length();
#ifdef SKINNED_SHADOWS
    if(skinned) count = skinnedNodes.length();
#endif
    while(i<count){
        RayNode node;
#ifdef SKINNED_SHADOWS
        if(skinned) node = skinnedNodes[i]; else
#endif
        node = nodes[i];
        if(!intersectsBox(origin,inverseDir,node,hit.distance)){
            i = node.count==0u ? node.first : i+1u;
            continue;
        }
        for(uint j=0u; j<node.count; ++j){
            uint id = node.first+j;
            Triangle t;
#ifdef SKINNED_SHADOWS
            if(skinned) t = skinnedTriangles[id]; else
#endif
            t = triangles[id];
            if(intersectsTriangle(origin,dir,id,t,hit) && anyHit) return hit;
        }
        ++i;
    }
    return hit;
}

Hit traceScene(vec3 origin, vec3 dir, float limit, bool anyHit) {
    return traceTree(origin,dir,limit,anyHit,false);
}

#ifdef LIGHTING_CONFIG
vec3 lightSample(vec2 randomPair) {
    vec2 offset = (randomPair*2.0-1.0) * lightRight.w;
    return lightPosition.xyz + lightRight.xyz * offset.x + lightUp.xyz * offset.y;
}

float illumination(vec3 p, vec3 n, vec3 target) {
    vec3 delta = target - p;
    float ds = dot(delta,delta);
    vec3 l = delta * inversesqrt(ds);
    float emitter = modes.x==0u ? max(dot(-l,lightForward.xyz),0.0) : 1.0;
    return lightPosition.w * max(dot(n,l),0.0) * emitter / max(ds, 0.1);
}

float sampleVisibility(vec3 p, vec3 n, vec3 target) {
    vec3 origin = p + n * lens.w;
    vec3 delta = target - origin;
    float distance = length(delta);
    if(distance<0.0001) return 1.0;
    return traceScene(origin, delta/distance, distance-0.001, true).id == 0xffffffffu ? 1.0 : 0.0;
}

#ifdef GI_MATERIALS
#include "gi_hit.glsl"
vec3 worldBounce(vec3 origin, vec3 ray, inout uint seed) {
    Hit hit=traceScene(origin,ray,9.0,false);
    if(hit.id==0xffffffffu) return vec3(0);
    Triangle t=triangles[hit.id];
    // Closed opaque surfaces do not emit diffusely through their backs.
    vec3 p=origin+ray*hit.distance, n=triangleNormal(t,hit.uv);
    HitMaterial m=hitMaterial(t.material.x);
    if(dot(n,ray)>0.0) {
        if(m.tex.w==0u) return vec3(0);
        n=-n;
    }
    vec3 target=lightSample(vec2(random(seed),random(seed)));
    HitResponse response=shadeGiHit(triangleUV(t,hit.uv),m);
    return response.emission + response.diffuse * illumination(p,n,target)*sampleVisibility(p,n,target);
}
#endif
#endif
