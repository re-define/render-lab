// Directional lights store sin(angular radius) in directionRange.w. Radiance is
// integrated over the source, so changing its extent preserves the energy budget.
vec3 visibleLight(Surface s,uint light,inout uint seed,uint diskSamples,float bias) {
    SceneLight lamp=lights[light];
    vec3 axis; float distanceToLight;
    lightAt(lamp,s.point.xyz,axis,distanceToLight);
    bool leaf=s.sky<0.;
    vec3 normal=surfaceNormal(s);
    vec3 origin=s.point.xyz+(leaf ? axis : normal)*bias;
    vec3 energy=lightAt(lamp,origin,axis,distanceToLight);
    float radius=lamp.positionKind.w==0. ? lamp.directionRange.w : 0.;
    if(radius==0.) {
        vec3 value=incident(s,light);
        if(luma(value)<=0.) return vec3(0);
        return distanceToLight<0.003 || !rayOccluded(origin,axis,distanceToLight-0.002) ? value : vec3(0);
    }
    if(!leaf && dot(axis,normal)<=-radius) return vec3(0);
    vec3 u=normalize(cross(abs(axis.y)<0.98 ? vec3(0,1,0) : vec3(1,0,0),axis));
    vec3 v=cross(axis,u);
    float height=radius*radius/(1.+sqrt(max(0.,1.-radius*radius)));
    float rotation=random(seed), radialOffset=random(seed);
    vec3 sum=vec3(0);
    for(uint j=0u;j<diskSamples;j++) {
        float h=height*(float(j)+radialOffset)/float(diskSamples);
        float angle=TAU*fract(rotation+float(j)*0.61803399);
        vec3 dir=axis*(1.-h)+sqrt(h*(2.-h))*(cos(angle)*u+sin(angle)*v);
        float cosine=dot(dir,normal);
        if((leaf || cosine>0.) && !rayOccluded(origin,dir,distanceToLight))
            sum+=surfaceResponse(s,cosine,0.);
    }
    return energy*(controls.z*sum/float(diskSamples));
}
