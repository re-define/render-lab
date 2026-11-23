#version 460
#extension GL_EXT_mesh_shader : require
layout(location=0) perprimitiveEXT flat in uvec2 id;
layout(location=0) out uvec2 visibility;
#ifdef PLANTS
layout(location=1) perprimitiveEXT flat in uvec2 transition;

#include "cluster_coverage.glsl"
#endif
void main(){
#ifdef PLANTS
    if(transition.x!=0xffff0000u && !coversLod(uvec2(gl_FragCoord.xy),transition))discard;
#endif
    visibility=id;
#ifdef COVERAGE_PROBE
    // Diagnostic targets use LESS_OR_EQUAL and clear to one. Every surviving
    // surface sample passes at zero, even if another primitive covered it.
    gl_FragDepth=0.;
#endif
}
