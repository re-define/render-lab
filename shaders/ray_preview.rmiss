#version 460
#extension GL_EXT_ray_tracing : require
#include "ray_preview.glsl"
layout(location=0) rayPayloadInEXT RayHit surface;
void main(){surface.distance=-1.0;}
