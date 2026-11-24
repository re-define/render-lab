// Both backends intersect the same ray interval. Shading and light sampling are the callers' work.
#ifdef SCENE_RAY_CLOSEST
#include "scene_ray_hit.glsl"
#endif
#ifdef SCENE_RAY_INLINE
layout(push_constant) uniform RayScene { layout(offset=TLAS_OFFSET) uvec2 tlas; } rayScene;
bool rayOccluded(vec3 origin, vec3 direction, float limit) {
    rayQueryEXT q;
    rayQueryInitializeEXT(q,accelerationStructureEXT(rayScene.tlas),gl_RayFlagsTerminateOnFirstHitEXT,255,origin,0.0001,direction,limit);
    while(rayQueryProceedEXT(q)) {}
    return rayQueryGetIntersectionTypeEXT(q,true)==gl_RayQueryCommittedIntersectionTriangleEXT;
}
#ifdef SCENE_RAY_CLOSEST
#include "ray_triangle_hit.glsl"
bool rayClosest(vec3 origin, vec3 direction, float limit, out SceneRayHit hit) {
    rayQueryEXT q;
    rayQueryInitializeEXT(q,accelerationStructureEXT(rayScene.tlas),gl_RayFlagsNoneEXT,255,origin,0.0001,direction,limit);
    while(rayQueryProceedEXT(q)) {}
    if(rayQueryGetIntersectionTypeEXT(q,true)!=gl_RayQueryCommittedIntersectionTriangleEXT) return false;
    vec2 bary=rayQueryGetIntersectionBarycentricsEXT(q,true);
    hit.distance=rayQueryGetIntersectionTEXT(q,true);
    triangleHit(uint(rayQueryGetIntersectionInstanceIdEXT(q,true)),uint(rayQueryGetIntersectionPrimitiveIndexEXT(q,true)),
        vec3(1.-bary.x-bary.y,bary),mat3(rayQueryGetIntersectionWorldToObjectEXT(q,true)),
        mat3(rayQueryGetIntersectionObjectToWorldEXT(q,true)),hit);
    return true;
}
#endif
#elif defined(SCENE_RAY_HARDWARE)
layout(push_constant) uniform RayScene { layout(offset=TLAS_OFFSET) uvec2 tlas; } rayScene;
#ifdef SCENE_RAY_CLOSEST
layout(location=0) rayPayloadEXT SceneRayHit rayResult;
#define SHADOW_STATUS rayResult.material
bool rayClosest(vec3 origin, vec3 direction, float limit, out SceneRayHit hit) {
    traceRayEXT(accelerationStructureEXT(rayScene.tlas), gl_RayFlagsNoneEXT, 255,0,0,0,
                origin,0.0001,direction,limit,0);
    if(rayResult.material==0xffffffffu) return false;
    hit=rayResult;
    return true;
}
#else
layout(location=0) rayPayloadEXT uint shadowStatus;
#define SHADOW_STATUS shadowStatus
#endif
bool rayOccluded(vec3 origin, vec3 direction, float limit) {
    SHADOW_STATUS=0u;
    traceRayEXT(accelerationStructureEXT(rayScene.tlas),
                gl_RayFlagsTerminateOnFirstHitEXT|gl_RayFlagsSkipClosestHitShaderEXT,
                255,0,0,0,origin,0.0001,direction,limit,0);
    return SHADOW_STATUS==0u;
}
#undef SHADOW_STATUS
#else
#include "lighting_rays.glsl"
#if defined(SKINNED_SHADOWS) && !defined(SCENE_HAS_DEFORMED)
#define SCENE_HAS_DEFORMED (skinnedNodes.length()!=0)
#endif
bool rayOccluded(vec3 origin, vec3 direction, float limit) {
#ifdef SKINNED_SHADOWS
    return traceTree(origin,direction,limit,true,false).id!=0xffffffffu ||
        (SCENE_HAS_DEFORMED && traceTree(origin,direction,limit,true,true).id!=0xffffffffu);
#else
    return traceTree(origin,direction,limit,true,false).id!=0xffffffffu;
#endif
}
#ifdef SCENE_RAY_CLOSEST
bool rayClosest(vec3 origin, vec3 direction, float limit, out SceneRayHit result) {
    Hit h=traceTree(origin,direction,limit,false,false);
    Triangle t;
#ifdef SKINNED_SHADOWS
    Hit deform=Hit(h.distance,0xffffffffu,vec2(0));
    if(SCENE_HAS_DEFORMED) deform=traceTree(origin,direction,h.distance,false,true);
    if(deform.id!=0xffffffffu) { h=deform; t=skinnedTriangles[h.id]; }
    else
#endif
    if(h.id!=0xffffffffu) t=triangles[h.id];
    else return false;
    result=SceneRayHit(triangleNormal(t,h.uv),h.distance,triangleUV(t,h.uv),t.material.x);
    return true;
}
#endif
#endif
