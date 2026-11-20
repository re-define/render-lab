struct Instance { mat4 model; vec4 color; vec4 params; uvec4 tex; uvec4 routing; };
struct Cluster { uvec4 span; vec4 sphere; uvec4 meta; };
struct Group { vec4 sphere; float error; uint level; uint firstCluster; uint clusterCount; };
struct Node { vec4 geometry; vec4 projection; uint children; uint first; uint group; float error; };
struct View { mat4 vp; vec4 eyeFocal; uvec4 screen; vec4 settings; };
const uint NONE=0xffffffffu;
bool outsidePlane(vec4 plane,vec4 sphere){
    return dot(plane,vec4(sphere.xyz,1.0)) < -(sphere.w+1e-5)*length(plane.xyz);
}
bool outsideFrustum(mat4 m,vec4 sphere){
    mat4 t=transpose(m);
    return outsidePlane(t[3]+t[0],sphere)||outsidePlane(t[3]-t[0],sphere)||
           outsidePlane(t[3]+t[1],sphere)||outsidePlane(t[3]-t[1],sphere)||
           outsidePlane(t[2],sphere)||outsidePlane(t[3]-t[2],sphere);
}
bool refine(float error,vec4 sphere,Instance inst,View view){
    if(view.settings.x==0.0 || error>=3.4028234e38) return true;
    float distanceToSphere=length((inst.model*vec4(sphere.xyz,1)).xyz-view.eyeFocal.xyz)-sphere.w*inst.params.z;
    return error*inst.params.z*view.eyeFocal.w > view.settings.x*max(view.settings.y,distanceToSphere);
}
