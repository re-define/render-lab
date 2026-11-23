struct SceneLight { vec4 positionKind; vec4 directionRange; vec4 radiance; };
layout(set=0,binding=SCENE_LIGHT_BINDING,std430) readonly buffer Lights {
    uvec4 lightHeader;
    SceneLight lights[];
};

vec3 skyEnergy(SceneLight light) { return light.radiance.rgb * light.radiance.w; }
// Returns directional irradiance before the surface cosine, and points toward the source.
vec3 lightAt(SceneLight light, vec3 position, out vec3 direction, out float distanceToLight) {
    if (light.positionKind.w == 0.0) {
        direction = light.directionRange.xyz;
        distanceToLight = 1e20;
        return light.radiance.rgb * (1.-light.radiance.w);
    }
    vec3 delta = light.positionKind.xyz - position;
    float square = dot(delta, delta);
    distanceToLight = sqrt(square);
    direction = delta * inversesqrt(max(square, 1e-9));
    float range = light.directionRange.w;
    float fade = 1.0;
    if (range > 0.0) {
        float q = min(distanceToLight / range, 1.0);
        fade = 1.0 - q*q*q*q;
    }
    return light.radiance.rgb * (fade * fade / max(square, 0.18));
}
