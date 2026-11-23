struct PlantVertex {
    vec4 p;
    vec4 n;
    uvec4 joints;
    vec4 weights;
};
layout(buffer_reference, std430, buffer_reference_align=16) readonly buffer PlantVertices { PlantVertex data[]; };
// Mirrors voxels::write_bricks. A brick and all its occupied cells share one pose.
layout(buffer_reference, std430, buffer_reference_align=4) readonly buffer BrickWords { uint word[]; };
struct PlantBrick { vec4 originSize; uvec2 occupancy; uint first; uint joint; uint bounds; };
struct PlantCell { vec2 uv; uvec3 lobes; uint location; };
PlantBrick loadBrick(BrickWords data, uint stream, uint index) {
    uint i=stream+1u+8u*index;
    return PlantBrick(uintBitsToFloat(uvec4(data.word[i],data.word[i+1],data.word[i+2],data.word[i+3])),
                      uvec2(data.word[i+4],data.word[i+5]),data.word[i+6]&65535u,data.word[i+7],data.word[i+6]>>16u);
}
uint cellAddress(BrickWords data, uint stream, uint slot) {
    return stream+1u+8u*data.word[stream]+6u*slot;
}
PlantCell loadCell(BrickWords data,uint stream,uint slot) {
    uint i=cellAddress(data,stream,slot);
    return PlantCell(uintBitsToFloat(uvec2(data.word[i],data.word[i+1])),
                     uvec3(data.word[i+2],data.word[i+3],data.word[i+4]),data.word[i+5]);
}
uvec3 brickGrid(uint location) {
    return (uvec3(location) >> uvec3(0,2,4)) & 3u;
}
vec4 cellBounds(PlantBrick brick,uint location) {
    float size=brick.originSize.w;
    return vec4(brick.originSize.xyz+(vec3(brickGrid(location))+.5)*size,.5*size);
}
vec3 cellNormal(PlantCell cell,uint face,uint sampleBits) {
    uvec3 weights=cell.lobes >> 24u;
    uint pick=((sampleBits & 65535u)*(weights.x+weights.y+weights.z))>>16u;
    uint packed=pick<weights.x?cell.lobes.x:(pick<weights.x+weights.y?cell.lobes.y:cell.lobes.z);
    vec3 n=unpackSnorm4x8(packed).xyz;
    return (face%2u==0u ? n[face/2u]>0. : n[face/2u]<0.) ? -n : n;
}
layout(buffer_reference, std430, buffer_reference_align=16) readonly buffer PlantPalette { mat4 data[]; };

mat4 plantMatrix(PlantVertex vertex, PlantPalette palette, uint base) {
    mat4 matrix = mat4(0.0);
    for (uint i = 0u; i < 4u; ++i) {
        if (vertex.weights[i] != 0.0)
            matrix += palette.data[base + vertex.joints[i]] * vertex.weights[i];
    }
    return matrix;
}

vec3 voxelCorner(vec4 cell, uint corner) {
    return cell.xyz + (vec3(uvec3(corner, corner >> 1, corner >> 2) & 1u) * 2.0 - 1.0) * cell.w;
}

uvec3 voxelIndices(uint faceWord, uint triangle) {
    const uvec4 faces[6] = uvec4[](
        uvec4(0,4,6,2), uvec4(1,3,7,5),
        uvec4(0,1,5,4), uvec4(2,6,7,3),
        uvec4(0,2,3,1), uvec4(4,5,7,6)
    );
    uvec4 corners = faces[faceWord & 7u];
    uvec3 indices = (triangle & 1u) == 0u ? corners.xyz : corners.xzw;
    return indices + 8u * (faceWord >> 3);
}
