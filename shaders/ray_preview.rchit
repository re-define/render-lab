#version 460
#extension GL_EXT_ray_tracing : require
#include "ray_preview.glsl"
#include "lighting_scene.glsl"
layout(set=0,binding=0,std430) readonly buffer Triangles { Triangle triangles[]; };
layout(location=0) rayPayloadInEXT RayHit surface;
hitAttributeEXT vec2 barycentrics;

void main() {
    Triangle t = triangles[gl_InstanceCustomIndexEXT + gl_PrimitiveID];
    vec3 n=triangleNormal(t,barycentrics);
    surface.normal=dot(n,gl_WorldRayDirectionEXT)>0.0 ? -n : n;
    surface.distance=gl_HitTEXT;
    surface.material=t.color;
}
