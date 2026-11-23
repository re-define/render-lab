#version 460
#extension GL_EXT_mesh_shader : require
#include "dissolve_field.glsl"
layout(location=0) perprimitiveEXT flat in uvec2 id;
layout(location=1) in vec3 restPosition;
layout(location=2) perprimitiveEXT flat in float threshold;
layout(location=0) out uvec2 visibility;
void main(){
    if(erosion(restPosition)<threshold) discard;
    visibility=id;
}
