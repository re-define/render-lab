vec3 shadeMaterial(vec3 albedo,vec3 N,vec3 V,vec3 L,vec2 parameters){
    float nl=max(dot(N,L),0.);
    return albedo*(nl>.7?1.:nl>.3?.64:.32);
}
