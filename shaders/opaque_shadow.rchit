#version 460
#extension GL_EXT_ray_tracing : require
// Opaque visibility rays skip closest hit. Only the miss shader changes their payload.
void main() {}
