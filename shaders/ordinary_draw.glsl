#extension GL_EXT_buffer_reference2 : require
#extension GL_EXT_buffer_reference_uvec2 : require

struct DrawRecord {
    mat4 clipFromVertex;
    vec4 lo;
    vec4 hi;
    uvec4 info; // Stable surface ID (zero based), triangle count, vertex count, skinned flag.
};
layout(buffer_reference, std430, buffer_reference_align=16) readonly buffer Vertices { vec4 data[]; };
layout(set=0, binding=0, std430) readonly buffer Records { DrawRecord draws[]; };
layout(set=0, binding=1, std430) readonly buffer VertexAddresses { uvec2 vertices[]; };
