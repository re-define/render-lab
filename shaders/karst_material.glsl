vec3 rockScan(vec2 uv,uvec3 image) {
    uvec2 size=image.yz;
    vec2 p=fract(uv)*vec2(size)-0.5;
    uvec2 lo=uvec2(ivec2(floor(p))+ivec2(size))%size, hi=(lo+1u)%size;
    vec2 f=fract(p);
    vec3 a=mix(unpackUnorm4x8(texels[image.x+lo.y*size.x+lo.x]).rgb,
               unpackUnorm4x8(texels[image.x+lo.y*size.x+hi.x]).rgb,f.x);
    vec3 b=mix(unpackUnorm4x8(texels[image.x+hi.y*size.x+lo.x]).rgb,
               unpackUnorm4x8(texels[image.x+hi.y*size.x+hi.x]).rgb,f.x);
    return pow(mix(a,b,f.y),vec3(2.2));
}

vec3 karstAlbedo(vec3 position,vec3 normal,uvec3 image) {
#ifdef LIMESTONE
    // All planes are anchored in world space; blend across changes of face orientation.
    vec3 weights=pow(abs(normal),vec3(3.0));
    weights/=max(dot(weights,vec3(1.)),0.0001);
    vec2 bed=vec2(dot(position.xz,vec2(0.76,0.65)),position.y);
    float broad=noise2(bed*0.24)*0.50+noise2(position.zy*0.48)*0.30+noise2(position.xy*1.20)*0.20;
    float grain=dot(weights,vec3(noise2(position.yz*4.2),noise2(position.zx*4.2),noise2(position.xy*4.2)));
    vec3 scan=weights.x*rockScan(position.zy/5.6,image)+weights.y*rockScan(position.xz/5.6,image)
             +weights.z*rockScan(position.xy/5.6,image);
    float runoff=noise2(vec2(bed.x*1.7+broad,position.y*0.14));
    float value=dot(scan,vec3(0.2126,0.7152,0.0722));
    vec3 stone=mix(vec3(value),scan,0.45)*vec3(1.78,1.73,1.60)*(0.78+0.34*broad+0.15*runoff);
    float moss=smoothstep(0.41,0.91,normal.y+(broad-0.54)*1.35+0.38*(grain-0.5));
    return mix(stone,stone*vec3(0.52,0.72,0.39),moss*0.83);
#else
    vec2 p=position.xz;
    float cover=noise2(p*0.065+noise2(p*0.17));
    vec2 rotated=mat2(0.8,0.6,-0.6,0.8)*p;
    vec3 scan=mix(rockScan(p/46.,image),rockScan(rotated/32.+vec2(0.61,0.27),image),smoothstep(0.25,0.75,cover));
    float value=dot(scan,vec3(0.2126,0.7152,0.0722));
    vec3 earth=mix(vec3(value),scan,0.36)*vec3(2.35,2.39,2.30);
    float moss=1.-smoothstep(0.34,0.72,cover+0.18*(noise2(p*1.7)-0.5));
    return mix(earth,earth*vec3(0.65,1.08,0.66),moss);
#endif
}
