struct Instance { mat4 model; vec4 color; vec4 params; uvec4 tex; uvec4 routing; };
struct Cluster { uvec4 span; vec4 sphere; uvec4 meta; };
struct Group { vec4 sphere; float error; uint level; uint firstCluster; uint clusterCount; };
struct Node { vec4 geometry; vec4 projection; uint children; uint first; uint group; float error; };
struct View { mat4 vp; vec4 eyeFocal; uvec4 screen; vec4 settings; vec4 foliage; vec4 casterRays[2]; mat4 inverseVP; };
const uint NONE=0xffffffffu;
const uint PLANT=64u;
const uint HAS_VOXEL=256u;
const uint GEOMETRY_VIEW=512u;
const uint TEMPORAL_REACTIVE=1024u;
const uint WIND_DEFORM=4096u;
const uint VOXEL_CLUSTER=1u;
bool outsidePlane(vec4 plane,vec4 sphere){
    return dot(plane,vec4(sphere.xyz,1.0)) < -(sphere.w+1e-5)*length(plane.xyz);
}
bool outsideFrustum(mat4 m,vec4 sphere){
    mat4 t=transpose(m);
    return outsidePlane(t[3]+t[0],sphere)||outsidePlane(t[3]-t[0],sphere)||
           outsidePlane(t[3]+t[1],sphere)||outsidePlane(t[3]-t[1],sphere)||
           outsidePlane(t[2],sphere)||outsidePlane(t[3]-t[2],sphere);
}
bool foliageLod(Instance inst){
    return (inst.tex.w & (PLANT | HAS_VOXEL)) == (PLANT | HAS_VOXEL);
}
float refinementWeight(float error,vec4 sphere,Instance inst,View view){
    if(view.settings.x==0.0 || error>=3.4028234e38) return 1.0;
    float distanceToSphere=length((inst.model*vec4(sphere.xyz,1)).xyz-view.eyeFocal.xyz)-sphere.w*inst.params.z;
    float distance = max(view.settings.y,distanceToSphere);
    float numerator = error*inst.params.z*view.eyeFocal.w;
    if(!foliageLod(inst)) return float(numerator > view.settings.x*distance);
    float target = view.foliage.x;
    return smoothstep(0.70*target, 1.30*target, numerator/distance);
}

// A caster outside a receiver plane cannot cast inside it if every direction
// away from this emitter remains outside. Keeping extra casters is conservative.
bool outsideCasterVolume(mat4 model, vec4 sphere, View view) {
    mat4 clip=view.vp*model;
    if(view.foliage.y==0.) return outsideFrustum(clip,sphere);
    if(view.foliage.z<0.) return false;
    mat4 t=transpose(view.vp);
    vec4 planes[6]=vec4[](t[3]+t[0],t[3]-t[0],t[3]+t[1],t[3]-t[1],t[2],t[3]-t[2]);
    uint rejected=0u, lights=uint(view.foliage.z);
    for(uint i=0u;i<6;i++) {
        if(!outsidePlane(transpose(model)*planes[i],sphere)) continue;
        for(uint j=0u;j<lights;j++){
            vec4 ray=view.casterRays[j];
            if(dot(planes[i].xyz,ray.xyz) < -ray.w*length(planes[i].xyz)) rejected|=1u<<j;
        }
    }
    return lights>0u && rejected==((1u<<lights)-1u);
}
