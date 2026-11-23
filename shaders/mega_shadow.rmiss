#version 460
#extension GL_EXT_ray_tracing : require
layout(location=0) rayPayloadInEXT uint missed;
void main(){missed=1u;}
