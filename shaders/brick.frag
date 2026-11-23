#version 460
#extension GL_EXT_mesh_shader : require
#extension GL_EXT_buffer_reference2 : require
#extension GL_EXT_buffer_reference_uvec2 : require
#include "cluster_coverage.glsl"
#include "plant_geometry.glsl"
layout(location=0) perprimitiveEXT flat in vec4 gridEyeNear;
layout(location=1) perprimitiveEXT flat in vec3 rayCenter;
layout(location=2) perprimitiveEXT flat in uvec4 masks;
layout(location=3) perprimitiveEXT flat in uvec4 ids;
layout(location=4) perprimitiveEXT flat in vec4 depthRow;
layout(location=5) perprimitiveEXT flat in vec4 wRow;
layout(location=6) perprimitiveEXT flat in uvec2 fade;
layout(location=7) perprimitiveEXT flat in vec3 rayX;
layout(location=8) perprimitiveEXT flat in vec3 rayY;
#include "brick_ray.glsl"
#ifndef COVERAGE_PROBE
layout(depth_greater) out float gl_FragDepth;
#endif
layout(location=0) out uvec2 visibility;

#include "brick_trace.glsl"
void main() {
    if(fade.x!=0xffff0000u && !coversLod(uvec2(gl_FragCoord.xy),fade)) discard;
#ifdef RECTANGLE_PROBE
    gl_FragDepth=0.;
    visibility=uvec2(1);
    return;
#endif
    float depth=gl_FragCoord.z;
    uint primitive=ids.y;
    if(ids.w==0u) {
        vec3 ray=brickPixelRay(mat3(rayX,rayY,rayCenter),uvec2(gl_FragCoord.xy));
        if(!hitBrick(gridEyeNear.xyz+gridEyeNear.w*ray,ray,masks,ids.z,ids.y,depthRow,wRow,depth,primitive)) discard;
    }
#ifdef COVERAGE_PROBE
    gl_FragDepth=0.;
#else
    gl_FragDepth=max(gl_FragCoord.z,depth);
#endif
    visibility=uvec2(ids.x,primitive);
}
