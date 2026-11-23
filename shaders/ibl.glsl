layout(set=0,binding=IBL_TABLES,std430) readonly buffer IblTables { uint table[]; };
layout(set=0,binding=IBL_CONFIG,std430) readonly buffer IblControls {
    mat4 inverseVP;
    vec4 cameraEye;
    vec4 controls; // exposure multiplier, yaw, display channel
    uvec4 dimensions;
};

vec3 iblRotate(vec3 direction) {
    float c=cos(controls.y), s=sin(controls.y);
    return vec3(c*direction.x-s*direction.z,direction.y,s*direction.x+c*direction.z);
}

vec3 iblTexel(uint offset, int w, ivec2 xy) {
    int x=xy.x&(w-1), y=clamp(xy.y,0,w/2-1);
    uint word=offset+2u*uint(y*w+x);
    return vec3(unpackHalf2x16(table[word]),unpackHalf2x16(table[word+1u]).x);
}

vec3 iblSample(uint header, vec3 direction) {
    uint offset=table[header];
    int w=int(table[header+1u]);
    vec3 d=iblRotate(direction);
    // Avoid undefined atan(0, 0) at the poles.
    float turn=abs(d.x)+abs(d.z)>1e-7 ? atan(d.z,d.x)*0.159154943 : 0.0;
    vec2 uv=vec2(turn+0.5,acos(clamp(d.y,-1.0,1.0))*0.318309886);
    vec2 coordinate=uv*vec2(w,w/2)-0.5;
    ivec2 p=ivec2(floor(coordinate));
    vec2 f=fract(coordinate);
    return mix(mix(iblTexel(offset,w,p),iblTexel(offset,w,p+ivec2(1,0)),f.x),
               mix(iblTexel(offset,w,p+ivec2(0,1)),iblTexel(offset,w,p+1),f.x),f.y);
}

vec2 iblBrdf(vec2 nvRoughness) {
    uint start=table[2];
    int w=int(table[3]);
    vec2 p=clamp(nvRoughness*float(w)-0.5,vec2(0),vec2(w-1));
    ivec2 a=ivec2(p), b=min(a+1,ivec2(w-1));
    vec2 f=fract(p);
    return mix(
        mix(unpackHalf2x16(table[start+uint(a.y*w+a.x)]),unpackHalf2x16(table[start+uint(a.y*w+b.x)]),f.x),
        mix(unpackHalf2x16(table[start+uint(b.y*w+a.x)]),unpackHalf2x16(table[start+uint(b.y*w+b.x)]),f.x),f.y);
}

vec3 iblToneMap(vec3 radiance) {
    vec3 x=max(radiance,0.0)*controls.x;
    return clamp((x*(2.51*x+0.03))/(x*(2.43*x+0.59)+0.14),0.0,1.0);
}
