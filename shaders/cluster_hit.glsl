uvec2 hitPixel(View view) {
    vec3 hit=gl_WorldRayOriginEXT+gl_HitTEXT*gl_WorldRayDirectionEXT;
    vec4 clip=view.vp*vec4(hit,1.);
    vec2 uv=clip.xy/max(abs(clip.w),1e-6)*0.5+0.5;
    // Signed pixel coordinates extend the same stable mask beyond the viewport.
    return uvec2(ivec2(floor(uv*vec2(view.screen.xy))));
}
bool coveredHit(uvec2 transition,View view) {
    return coversLod(hitPixel(view),transition);
}
