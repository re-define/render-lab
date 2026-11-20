// Link this diagnostic directly to the upstream forward.cu and backward.cu.
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cuda_runtime.h>
#include "forward.h"
#include "backward.h"

static void check(cudaError_t status) {
    if (status != cudaSuccess) {
        std::fprintf(stderr, "%s\n", cudaGetErrorString(status));
        std::exit(1);
    }
}

template<class T> T* zeros(int count) {
    T* values;
    check(cudaMallocManaged(&values, count*sizeof(T)));
    check(cudaMemset(values, 0, count*sizeof(T)));
    check(cudaDeviceSynchronize());
    return values;
}

int main() {
    constexpr int w=32, h=16, n=w*h, pixel=6*w+8;
    dim3 grid(2,1), block(16,16);
    auto xy=zeros<float2>(1), scale=zeros<float2>(1), projected=zeros<float2>(1);
    auto angle=zeros<float>(1), opacity=zeros<float>(1), multiplier=zeros<float>(1);
    auto rgb=zeros<float>(3), bg=zeros<float>(3), cov=zeros<float>(3);
    auto conic=zeros<float4>(1);
    auto radii=zeros<int>(1);
    auto touched=zeros<unsigned>(1), list=zeros<unsigned>(1), count=zeros<unsigned>(n);
    auto ranges=zeros<uint2>(2);
    auto trans=zeros<float>(n), output=zeros<float>(3*n), upstream=zeros<float>(3*n);
    auto dxy=zeros<float2>(1), dscale=zeros<float2>(1);
    auto dconic=zeros<float3>(1);
    auto drot=zeros<float>(1), dop=zeros<float>(1), dmult=zeros<float>(1), drgb=zeros<float>(3);
    *xy={8.7f,6.5f}; *scale={2.3f,1.4f}; *angle=.4f; *opacity=.7f;
    *multiplier=1.f; rgb[0]=.8f; upstream[pixel]=1.f;
    ranges[0]=ranges[1]=make_uint2(0,1);
    auto forward=[&]() {
        FORWARD::preprocess(1,(float*)xy,(glm::vec2*)scale,angle,multiplier,opacity,rgb,w,h,radii,cov,projected,conic,grid,touched,false,false);
        FORWARD::render(multiplier,grid,block,ranges,list,w,h,projected,rgb,conic,trans,count,bg,output);
        check(cudaDeviceSynchronize());
        return output[pixel];
    };
    forward();
    BACKWARD::render(multiplier,dmult,grid,block,ranges,list,w,h,bg,projected,conic,rgb,trans,count,upstream,dxy,dconic,dop,drgb);
    BACKWARD::cov2d(1,scale,angle,dconic,dscale,drot);
    check(cudaDeviceSynchronize());
    auto compare=[&](const char* name,float* parameter,float gradient) {
        const float v=*parameter, eps=.001f;
        *parameter=v+eps; float plus=forward();
        *parameter=v-eps; float minus=forward();
        *parameter=v;
        std::printf("%s: CUDA backward %.7f, central difference %.7f\n",name,gradient,(plus-minus)/(2*eps));
    };
    compare("center.x",&xy->x,dxy->x);
    compare("center.y",&xy->y,dxy->y);
    compare("scale.x",&scale->x,dscale->x);
    compare("scale.y",&scale->y,dscale->y);
    compare("rotation",angle,*drot);
    compare("opacity",opacity,*dop);
    compare("multiplier",multiplier,*dmult);
    return 0;
}
