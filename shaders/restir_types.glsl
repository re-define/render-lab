// 32-byte std430 receiver. Octahedral encoding leaves a word for sky visibility.
// Thin two-sided foliage stores -(1 + visibility); all visibility item IDs stay intact.
struct Surface { vec4 point; vec2 normalOct; float sky; uint id; };
vec2 octSign(vec2 v) { return mix(vec2(-1.),vec2(1.),greaterThanEqual(v,vec2(0.))); }
vec2 encodeSurfaceNormal(vec3 n) {
    n/=abs(n.x)+abs(n.y)+abs(n.z);
    return n.z>=0. ? n.xy : (1.-abs(n.yx))*octSign(n.xy);
}
vec3 surfaceNormal(Surface s) {
    vec3 n=vec3(s.normalOct,1.-abs(s.normalOct.x)-abs(s.normalOct.y));
    if(n.z<0.) n.xy=(1.-abs(n.yx))*octSign(n.xy);
    return normalize(n);
}
float receiverSky(vec3 n, bool leaf, float foliageU) {
    return leaf && foliageU>1.5 ? mix(0.22,1.0,clamp(foliageU-2.,0.,1.)) : 0.48+0.52*max(n.y,0.);
}
Surface makeReceiver(vec4 point,vec3 n,uint id,bool leaf,float sky) {
    return Surface(point,encodeSurfaceNormal(n),leaf ? -1.-sky : sky,id);
}
float skyVisibility(Surface s) { return s.sky<0. ? -1.-s.sky : s.sky; }
struct Reservoir { uint light; uint count; float sum; float target; };
struct Radiance { vec4 color; vec4 moments; }; // RGB/mean, second moment/age/variance
// Compact std430 history, 64 bytes per pixel. Depth is device Z.
struct Previous { vec3 normal; uint tag; Reservoir reservoir; vec4 color; vec3 moments; float depth; };

float luma(vec3 rgb) { return dot(rgb,vec3(0.2126,0.7152,0.0722)); }
Reservoir emptyReservoir() { return Reservoir(0u,0u,0.0,0.0); }
Radiance measure(vec3 rgb) {
    float l=luma(rgb);
    return Radiance(vec4(rgb,l),vec4(l*l,1.0,l*l,0.0));
}

bool compatible(Surface a,Surface b,float radius) {
    if(a.id==0u || a.id!=b.id || dot(surfaceNormal(a),surfaceNormal(b))<0.93) return false;
    vec3 delta=a.point.xyz-b.point.xyz;
    return abs(dot(delta,surfaceNormal(a)))<0.035+0.003*a.point.w && dot(delta,delta)<radius*radius;
}
