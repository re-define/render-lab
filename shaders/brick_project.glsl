#include "brick_ray.glsl"
mat3 brickRayBasis(mat3 toLocal,float size) {
    mat4 inv=view.inverseVP;
    vec3 eye=view.eyeFocal.xyz;
    vec3 x=toLocal*(inv[0].xyz-eye*inv[0].w)/size;
    vec3 y=toLocal*(inv[1].xyz-eye*inv[1].w)/size;
    vec4 center=inv[2]+inv[3];
    vec3 z=toLocal*(center.xyz-eye*center.w)/size;
    vec2 stepSize=2./vec2(view.screen.xy);
    return mat3(x*stepSize.x,y*stepSize.y,z+x*(.5*stepSize.x-1.)+y*(.5*stepSize.y-1.));
}
// Pixel-center bounds and conservative nearest depth shared by both rasterizers.
uvec4 projectBrick(PlantBrick brick,Instance obj,out mat4 clip,out mat3 toLocal,out vec3 eyeGrid,out float nearest) {
    mat4 pose=(obj.tex.w&WIND_DEFORM)!=0u ? jointPalette[obj.routing.w+brick.joint] : mat4(1);
    mat4 grid=mat4(brick.originSize.w);
    grid[3]=vec4(brick.originSize.xyz,1.);
    mat4 model=obj.model*pose;
    clip=view.vp*model*grid;
    toLocal=inverse(mat3(model));
    eyeGrid=(toLocal*(view.eyeFocal.xyz-model[3].xyz)-brick.originSize.xyz)/brick.originSize.w;
    uvec3 lo=brickGrid(brick.bounds),hi=brickGrid(brick.bounds>>6u)+1u;
    vec2 low=vec2(1),high=vec2(-1);
    float minDepth=1.,maxZ=-1.;
    bool clipped=false;
    for(uint k=0u;k<8u;k++) {
        uvec3 bits=(uvec3(k)>>uvec3(0,1,2))&1u;
        vec3 corner=mix(vec3(lo),vec3(hi),vec3(bits));
        vec4 h=clip*vec4(corner,1.);
        maxZ=max(maxZ,h.z);
        if(h.w<=0. || h.z<=0.) { clipped=true; continue; }
        vec3 ndc=h.xyz/h.w;
        low=min(low,ndc.xy); high=max(high,ndc.xy); minDepth=min(minDepth,ndc.z);
    }
    if(maxZ<=0.) { nearest=0.; return uvec4(0); }
    if(clipped) {
        // Clip the box's 12 edges against the near plane. A branch crossing
        // that plane need not enqueue the entire viewport.
        for(uint axis=0u;axis<3u;axis++) for(uint k=0u;k<4u;k++) {
            uvec3 bits=uvec3(0);
            bits[(axis+1u)%3u]=k&1u;
            bits[(axis+2u)%3u]=k>>1u;
            vec4 a=clip*vec4(mix(vec3(lo),vec3(hi),vec3(bits)),1.);
            bits[axis]=1u;
            vec4 b=clip*vec4(mix(vec3(lo),vec3(hi),vec3(bits)),1.);
            if((a.z>0.)==(b.z>0.)) continue;
            vec4 nearPoint=mix(a,b,a.z/(a.z-b.z));
            if(nearPoint.w<=0.) { low=vec2(-1); high=vec2(1); }
            else { vec2 p=nearPoint.xy/nearPoint.w; low=min(low,p); high=max(high,p); }
        }
    }
    vec2 pixels=vec2(view.screen.xy);
    // Include every pixel center inside the continuous bounds. Rounding up to
    // pixel edges keeps the proxy conservative with no fractional guard band.
    vec2 a=clamp(ceil((low*.5+.5)*pixels-.504),vec2(0),pixels);
    vec2 z=clamp(floor((high*.5+.5)*pixels-.496)+1.,vec2(0),pixels);
    nearest=clipped?0.:uintBitsToFloat(max(floatBitsToUint(minDepth),16u)-16u);
    return uvec4(a,max(z-a,vec2(0)));
}
