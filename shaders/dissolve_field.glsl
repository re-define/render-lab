// A continuous, material-space erosion field, shared by visibility and edge shading.
float mineralHash(vec3 p){
    p=fract(p*vec3(.1031,.1030,.0973));
    p+=dot(p,p.yxz+33.33);
    return fract((p.x+p.y)*p.z);
}
float mineralNoise(vec3 p){
    vec3 i=floor(p), f=fract(p);
    f=f*f*(3.-2.*f);
    return mix(mix(mix(mineralHash(i),mineralHash(i+vec3(1,0,0)),f.x),
                   mix(mineralHash(i+vec3(0,1,0)),mineralHash(i+vec3(1,1,0)),f.x),f.y),
               mix(mix(mineralHash(i+vec3(0,0,1)),mineralHash(i+vec3(1,0,1)),f.x),
                   mix(mineralHash(i+vec3(0,1,1)),mineralHash(i+vec3(1,1,1)),f.x),f.y),f.z);
}
float erosion(vec3 p){
    return .24+.22*p.y+.09*p.x+.42*(mineralNoise(p*3.4)-.5)+.15*(mineralNoise(p*9.7)-.5);
}
