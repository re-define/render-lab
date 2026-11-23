#define SCENE_LIGHT_BINDING 1
#include "scene_lights.glsl"
#include "leaf_light.glsl"

vec3 surfaceResponse(Surface s, float cosine, float radius) {
    if (s.sky<0.) return leafResponse(cosine);
    float c = radius > 0.0 && abs(cosine) < radius
        ? (cosine+radius)*(cosine+radius)/(4.0*radius) : max(cosine,0.0);
    return vec3(c);
}

vec3 incident(Surface surface,uint light) {
    vec3 direction; float distanceToLight;
    vec3 energy=lightAt(lights[light],surface.point.xyz,direction,distanceToLight);
    // The sampling target must have support even when only the rim is above the horizon.
    float radius=lights[light].positionKind.w==0. ? lights[light].directionRange.w : 0.;
    float cosine=dot(direction,surfaceNormal(surface));
    return energy * surfaceResponse(surface,cosine,radius) * controls.z;
}
uint sampleLight(inout uint seed) { return min(uint(random(seed)*float(options.y)),options.y-1u); }
void offer(inout Reservoir r,uint chosen,float weight,uint count,float target,inout uint seed) {
    r.sum+=weight;
    r.count+=count;
    if(weight>0.0 && random(seed)*r.sum<weight) {
        r.light=chosen;
        r.target=target;
    }
}
float normalization(Reservoir r) {
    return r.target>0.0 ? r.sum/(float(r.count)*r.target) : 0.0;
}
void reuse(inout Reservoir r,Reservoir other,Surface receiver,uint limit,inout uint seed) {
    uint count=min(other.count,limit);
    if(count==0u) return;
    float target=luma(incident(receiver,other.light));
    offer(r,other.light,target*normalization(other)*float(count),count,target,seed);
}
