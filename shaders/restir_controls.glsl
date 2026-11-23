layout(set=0,binding=RESTIR_CONFIG,std430) readonly buffer RestirConfig {
    vec4 reprojection; // previous minus current jitter in render pixels
    uvec4 options; // sampling mode, point lights, initial candidates, reference samples
    uvec4 viewport;
    vec4 controls; // exposure multiplier, denoiser
    vec4 canopySky; // summed diffuse sky irradiance of active scene lights, linear RGB
};
#define screenSize viewport.xy

uint indexAt(uvec2 xy) { return xy.y*screenSize.x+xy.x; }
bool inScreen(ivec2 xy) {
    return all(greaterThanEqual(xy,ivec2(0))) && all(lessThan(xy,ivec2(screenSize)));
}
