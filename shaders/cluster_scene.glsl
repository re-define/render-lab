#extension GL_EXT_buffer_reference2 : require
#extension GL_EXT_buffer_reference_uvec2 : require
#include "cluster.glsl"

layout(buffer_reference, std430) readonly buffer Nodes { Node data[]; };
layout(buffer_reference, std430) readonly buffer LodGroups { Group data[]; };
layout(buffer_reference, std430) readonly buffer Meshlets { Cluster data[]; };
layout(buffer_reference, std430) readonly buffer Vertices { vec4 data[]; };
layout(buffer_reference, std430, buffer_reference_align=4) readonly buffer Indices { uint data[]; };
// Five device addresses per asset, in the order registered by ClusterScene.
struct Asset { Nodes tree; LodGroups groups; Meshlets clusters; Vertices vertices; Indices indices; };
layout(set=0, binding=0, std430) readonly buffer Assets { Asset assets[]; };
layout(set=0, binding=1, std430) readonly buffer Instances { Instance instances[]; };

const uint ROW_GROUPS = 32768u;
const uint NODE_FANOUT = 8u;
const uint NODES_PER_GROUP = 64u / NODE_FANOUT;
uint linearGroup() { return gl_WorkGroupID.x + ROW_GROUPS * gl_WorkGroupID.y; }
uint workIndex() {
    return linearGroup() * 64u
        + gl_LocalInvocationID.x;
}
// Only the reservation containing the first item of a new workgroup publishes its extent.
// Argument buffers start at (0, 1, 1), so an empty list dispatches no work.
#define PUBLISH_WORK(args, first, count, width) { \
    uint lastGroup = ((first) + (count) - 1u) / (width); \
    if (((first) % (width)) == 0u || lastGroup != (first) / (width)) { \
        atomicMax((args).x, min(lastGroup + 1u, ROW_GROUPS)); \
        if (lastGroup >= ROW_GROUPS) atomicMax((args).y, lastGroup / ROW_GROUPS + 1u); \
    } \
}
