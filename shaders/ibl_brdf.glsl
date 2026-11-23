vec3 environmentLighting(vec3 albedo,vec3 n,vec3 v,vec2 params) {
    float metal=clamp(params.x,0.0,1.0), roughness=clamp(params.y,0.0,1.0);
    float nv=clamp(dot(n,v),0.001,1.0);
    vec3 f0=mix(vec3(0.04),albedo,metal);
    vec3 f=f0+(max(vec3(1.0-roughness),f0)-f0)*pow(1.0-nv,5.0);
    vec3 diffuse=iblSample(0u,n)*albedo*(1.0-f)*(1.0-metal);
    vec3 reflection=reflect(-v,n);
    float level=roughness*6.0;
    uint lo=uint(level), hi=min(lo+1u,6u);
    vec3 filtered=mix(iblSample(4u+2u*lo,reflection),iblSample(4u+2u*hi,reflection),fract(level));
    vec2 brdf=iblBrdf(vec2(nv,roughness));
    vec3 specular=filtered*(f0*brdf.x+brdf.y);
    if(controls.z==1.0) return diffuse;
    if(controls.z==2.0) return specular;
    return diffuse+specular;
}
