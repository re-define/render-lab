vec3 shadeMaterial(vec3 albedo,vec3 N,vec3 V,vec3 L,vec2 parameters){
    float nl=max(dot(N,L),0.);
    vec3 H=normalize(V+L);float nv=max(dot(N,V),.001),nh=max(dot(N,H),0.);
    float rough=clamp(parameters.y,.06,1.),a2=pow(rough,4.),d=nh*nh*(a2-1.)+1.;
    float D=a2/(3.14159265*d*d),k=(rough+1.)*(rough+1.)/8.;
    float G=(nl/(nl*(1.-k)+k))*(nv/(nv*(1.-k)+k));
    vec3 F0=mix(vec3(.04),albedo,parameters.x),F=F0+(1.-F0)*pow(1.-max(dot(V,H),0.),5.);
    return albedo*.14+nl*3.*((1.-F)*(1.-parameters.x)*albedo/3.14159265+F*D*G/max(4.*nl*nv,.001));
}
