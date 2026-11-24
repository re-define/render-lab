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
#include "ray_triangle_hit.glsl"
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
    triangleHit(instance,primitive,b,mat3(gl_WorldToObjectEXT),mat3(gl_ObjectToWorldEXT),result);
}
