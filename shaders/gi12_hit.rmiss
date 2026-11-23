#version 460
#extension GL_EXT_ray_tracing : require
#include "scene_ray_hit.glsl"
layout(location=0) rayPayloadInEXT SceneRayHit result;
void main() { result.material=0xffffffffu; }
