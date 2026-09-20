#include <metal_stdlib>
using namespace metal;
struct Options { uint width; uint height; uint turns; uint srgb; };
float3 rgb(texture2d<float, access::sample> source, uint2 p, constant Options& o) {
    constexpr sampler s(coord::normalized, address::clamp_to_edge, filter::nearest);
    float2 uv=(float2(p)+0.5)/float2(o.width,o.height);
    switch (o.turns % 4) {
        case 1: uv=float2(1.0-uv.y,uv.x); break;
        case 2: uv=1.0-uv; break;
        case 3: uv=float2(uv.y,1.0-uv.x); break;
    }
    float3 c=source.sample(s,uv).rgb;
    if (o.srgb) c=select(1.055*pow(max(c,0.0),float3(1.0/2.4))-0.055,12.92*c,c<=0.0031308);
    return c;
}
kernel void convert(texture2d<float, access::sample> source [[texture(0)]],
                    texture2d<float, access::write> luma [[texture(1)]],
                    texture2d<float, access::write> chroma [[texture(2)]],
                    constant Options& o [[buffer(0)]], uint2 cell [[thread_position_in_grid]]) {
    if (cell.x>=chroma.get_width() || cell.y>=chroma.get_height()) return;
    float3 average=0;
    for (uint y=0;y<2;y++) for (uint x=0;x<2;x++) {
        uint2 p=cell*2+uint2(x,y);
        float3 c=rgb(source,p,o); average+=c*0.25;
        float value=(16.0+219.0*dot(c,float3(0.2126,0.7152,0.0722)))/255.0;
        luma.write(float4(value,0,0,1),p);
    }
    float u=(128.0+224.0*dot(average,float3(-0.114572,-0.385428,0.5)))/255.0;
    float v=(128.0+224.0*dot(average,float3(0.5,-0.454153,-0.045847)))/255.0;
    chroma.write(float4(u,v,0,1),cell);
}
