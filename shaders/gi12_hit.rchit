#version 460
#extension GL_EXT_ray_tracing : require
#extension GL_EXT_buffer_reference2 : require
#extension GL_EXT_buffer_reference_uvec2 : require
#ifdef GI_CLUSTER_HIT
#extension GL_NV_cluster_acceleration_structure : require
#include "surface.glsl"
layout(binding=11,std430) readonly buffer Cut { uvec4 clusters[]; };
layout(binding=12,std430) readonly buffer CutView { View cutView; };
#include "cluster_hit.glsl"
#endif
#include "scene_ray_hit.glsl"
layout(location=0) rayPayloadInEXT SceneRayHit result;
hitAttributeEXT vec2 bary;
struct RayVertex { vec4 positionU; vec4 normalV; };
layout(buffer_reference,std430,buffer_reference_align=16) readonly buffer HitVertices { RayVertex vertices[]; };
layout(buffer_reference,std430,buffer_reference_align=16) readonly buffer HitFaces { uvec4 faces[]; };
layout(binding=5,std430) readonly buffer Geometry { uvec2 streams[]; };
layout(binding=6,std430) readonly buffer Materials { uint regularCount; uint slots[]; };
void main() {
    vec3 b=vec3(1.-bary.x-bary.y,bary);
    uint instance=uint(gl_InstanceID), primitive=uint(gl_PrimitiveID);
    result.distance=gl_HitTEXT;
#ifdef GI_CLUSTER_HIT
    if(instance==regularCount) {
        uvec4 cut=clusters[gl_ClusterIDNV];
        mat3x4 clip; mat3 pos,normals; mat3x2 uv; uint mask;
        surfaceTriangle(items[cut.y],(cut.z<<6u)|primitive,hitPixel(cutView),clip,pos,normals,uv,mask);
        vec3 n=normals*b;
        result.normal=normalize(dot(n,n)>1e-10 ? n : cross(pos[1]-pos[0],pos[2]-pos[0]));
        result.uv=uv*b;
        result.material=cut.w;
        return;
    }
#endif
    uvec4 face=HitFaces(streams[2u*instance+1u]).faces[primitive];
    HitVertices source=HitVertices(streams[2u*instance]);
    mat3 normalToWorld=transpose(mat3(gl_WorldToObjectEXT));
    mat3 pos; vec3 n=vec3(0); vec2 uv=vec2(0);
    for(uint j=0;j<3u;++j) {
        RayVertex v=source.vertices[face[j]];
        pos[j]=v.positionU.xyz;
        vec3 transformed=normalToWorld*v.normalV.xyz;
        if(dot(transformed,transformed)>1e-16) n+=b[j]*normalize(transformed);
        uv+=b[j]*vec2(v.positionU.w,v.normalV.w);
    }
    if(dot(n,n)<=1e-10) {
        mat3 toWorld=mat3(gl_ObjectToWorldEXT);
        n=cross(toWorld*(pos[1]-pos[0]),toWorld*(pos[2]-pos[0]));
    }
    result.normal=normalize(n);
    result.uv=uv;
    result.material=slots[instance]==0xffffffffu ? face.w : slots[instance];
}
