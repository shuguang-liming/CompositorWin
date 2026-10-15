struct Params {
    canvas: vec4<f32>,
    layer: vec4<f32>,
    rotate: vec4<f32>,
    options: vec4<f32>,
    mask: vec4<f32>,
    mask_rotate: vec4<f32>,
    mask_options: vec4<f32>,
}
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var backdrop: texture_2d<f32>;
@group(0) @binding(2) var coverage: texture_2d<f32>;
@group(0) @binding(3) var destination: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(5) var<uniform> p: Params;

fn coordinates(pos: vec2<f32>, box: vec4<f32>, r: vec4<f32>) -> vec2<f32> {
    let d = pos - box.xy - box.zw * 0.5;
    return vec2<f32>(r.x * d.x + r.y * d.y, -r.y * d.x + r.x * d.y) / box.zw * r.zw + 0.5;
}
fn lum(c:vec3<f32>)->f32 {return dot(c,vec3<f32>(0.3,0.59,0.11));}
fn sat(c:vec3<f32>)->f32 {return max(max(c.r,c.g),c.b)-min(min(c.r,c.g),c.b);}
fn clip_color(c0:vec3<f32>)->vec3<f32>{
    var c=c0;let l=lum(c);let n=min(min(c.r,c.g),c.b);let x=max(max(c.r,c.g),c.b);
    if(n<0.0){c=vec3<f32>(l)+(c-vec3<f32>(l))*l/max(l-n,0.000001);}
    if(x>1.0){c=vec3<f32>(l)+(c-vec3<f32>(l))*(1.0-l)/max(x-l,0.000001);}
    return c;
}
fn set_lum(c:vec3<f32>,l:f32)->vec3<f32>{return clip_color(c+vec3<f32>(l-lum(c)));}
fn set_sat(c:vec3<f32>,s:f32)->vec3<f32>{
    let lo=min(min(c.r,c.g),c.b);let hi=max(max(c.r,c.g),c.b);
    return (c-vec3<f32>(lo))*s/max(hi-lo,0.000001);
}
fn burn(a:vec3<f32>,b:vec3<f32>)->vec3<f32>{
    let value=1.0-min(vec3<f32>(1.0),(1.0-a)/max(b,vec3<f32>(0.000001)));
    return select(select(value,vec3<f32>(0.0),b<=vec3<f32>(0.0)),vec3<f32>(1.0),a>=vec3<f32>(1.0));
}
fn dodge(a:vec3<f32>,b:vec3<f32>)->vec3<f32>{
    let value=min(vec3<f32>(1.0),a/max(1.0-b,vec3<f32>(0.000001)));
    return select(select(value,vec3<f32>(1.0),b>=vec3<f32>(1.0)),vec3<f32>(0.0),a<=vec3<f32>(0.0));
}
fn blend(a:vec3<f32>,b:vec3<f32>,mode:u32)->vec3<f32>{
    switch mode {
        case 1u: {return min(a,b);}
        case 2u: {return a*b;}
        case 3u: {return burn(a,b);}
        case 4u: {return max(vec3<f32>(0.0),a+b-1.0);}
        case 5u: {return max(a,b);}
        case 6u: {return 1.0-(1.0-a)*(1.0-b);}
        case 7u: {return dodge(a,b);}
        case 8u: {return min(vec3<f32>(1.0),a+b);}
        case 9u: {return select(2.0*a*b,1.0-2.0*(1.0-a)*(1.0-b),a>vec3<f32>(0.5));}
        case 10u: {
            let d=select(((16.0*a-12.0)*a+4.0)*a,sqrt(a),a>vec3<f32>(0.25));
            return select(a-(1.0-2.0*b)*a*(1.0-a),a+(2.0*b-1.0)*(d-a),b>vec3<f32>(0.5));
        }
        case 11u: {return select(2.0*a*b,1.0-2.0*(1.0-a)*(1.0-b),b>vec3<f32>(0.5));}
        case 12u: {return select(burn(a,2.0*b),dodge(a,2.0*b-1.0),b>vec3<f32>(0.5));}
        case 13u: {return clamp(a+2.0*b-1.0,vec3<f32>(0.0),vec3<f32>(1.0));}
        case 14u: {return select(min(a,2.0*b),max(a,2.0*b-1.0),b>vec3<f32>(0.5));}
        case 15u: {let v=select(burn(a,2.0*b),dodge(a,2.0*b-1.0),b>vec3<f32>(0.5));return select(vec3<f32>(0.0),vec3<f32>(1.0),v>=vec3<f32>(0.5));}
        case 16u: {return abs(a-b);}
        case 17u: {return a+b-2.0*a*b;}
        case 18u: {return max(vec3<f32>(0.0),a-b);}
        case 19u: {return select(min(vec3<f32>(1.0),a/max(b,vec3<f32>(0.000001))),vec3<f32>(1.0),b<=vec3<f32>(0.0));}
        case 20u: {return set_lum(set_sat(b,sat(a)),lum(a));}
        case 21u: {return set_lum(set_sat(a,sat(b)),lum(a));}
        case 22u: {return set_lum(b,lum(a));}
        case 23u: {return set_lum(a,lum(b));}
        default: {return b;}
    }
}
fn source_load(point:vec2<i32>,level:u32)->vec4<f32> {
    let dims=vec2<i32>(textureDimensions(source,level));
    if(any(point<vec2<i32>(0))||any(point>=dims)){return vec4<f32>(0.0);}
    return textureLoad(source,point,i32(level));
}
fn source_bilinear(uv:vec2<f32>,level:u32)->vec4<f32> {
    let point=uv*vec2<f32>(textureDimensions(source,level))-0.5;
    let corner=vec2<i32>(floor(point));let f=fract(point);
    return mix(mix(source_load(corner,level),source_load(corner+vec2<i32>(1,0),level),f.x),mix(source_load(corner+vec2<i32>(0,1),level),source_load(corner+vec2<i32>(1,1),level),f.x),f.y);
}
fn cubic_weight(distance:f32)->f32 {
    let t=abs(distance);
    if(t<=1.0){return ((1.5*t-2.5)*t)*t+1.0;}
    if(t<2.0){return ((-0.5*t+2.5)*t-4.0)*t+2.0;}
    return 0.0;
}
fn source_bicubic(uv:vec2<f32>,level:u32)->vec4<f32> {
    let point=uv*vec2<f32>(textureDimensions(source,level))-0.5;
    let corner=vec2<i32>(floor(point));let f=fract(point);
    var value=vec4<f32>(0.0);
    for(var y=-1;y<=2;y++){for(var x=-1;x<=2;x++){
        let weight=cubic_weight(f.x-f32(x))*cubic_weight(f.y-f32(y));
        if(weight!=0.0){value+=source_load(corner+vec2<i32>(x,y),level)*weight;}
    }}
    let alpha=clamp(value.a,0.0,1.0);
    let rescale=select(1.0,alpha/max(value.a,0.000001),value.a>1.0);
    return vec4<f32>(clamp(value.rgb*rescale,vec3<f32>(0.0),vec3<f32>(alpha)),alpha);
}
fn mask_load(point:vec2<i32>)->f32 {
    let dims=vec2<i32>(textureDimensions(coverage));
    if(any(point<vec2<i32>(0))||any(point>=dims)){return p.mask_options.x;}
    return textureLoad(coverage,point,0).r;
}
fn mask_bilinear(uv:vec2<f32>)->f32 {
    if(all(textureDimensions(coverage)==vec2<u32>(1u))){return textureLoad(coverage,vec2<i32>(0),0).r;}
    let point=uv*vec2<f32>(textureDimensions(coverage))-0.5;
    let corner=vec2<i32>(floor(point));let f=fract(point);
    return mix(mix(mask_load(corner),mask_load(corner+vec2<i32>(1,0)),f.x),mix(mask_load(corner+vec2<i32>(0,1)),mask_load(corner+vec2<i32>(1,1)),f.x),f.y);
}
fn mask_sample(uv:vec2<f32>)->f32 {
    if(all(textureDimensions(coverage)==vec2<u32>(1u))){return textureLoad(coverage,vec2<i32>(0),0).r;}
    if(p.mask_options.y<0.5){return mask_load(vec2<i32>(floor(uv*vec2<f32>(textureDimensions(coverage)))));}
    if(p.mask_options.y<1.5){return mask_bilinear(uv);}
    let point=uv*vec2<f32>(textureDimensions(coverage))-0.5;
    let corner=vec2<i32>(floor(point));let f=fract(point);
    var value=0.0;
    for(var y=-1;y<=2;y++){for(var x=-1;x<=2;x++){
        value+=mask_load(corner+vec2<i32>(x,y))*cubic_weight(f.x-f32(x))*cubic_weight(f.y-f32(y));
    }}
    return clamp(value,0.0,1.0);
}
@compute @workgroup_size(8,8)
fn composite(@builtin(global_invocation_id) id:vec3<u32>) {
    let dims=textureDimensions(destination);
    if(any(id.xy>=dims)){return;}
    let pos=(vec2<f32>(id.xy)+0.5)/vec2<f32>(dims)*p.canvas.xy;
    let uv=coordinates(pos,p.layer,p.rotate);
    var s=vec4<f32>(0.0);
    if(p.options.w<0.5){s=source_load(vec2<i32>(floor(uv*vec2<f32>(textureDimensions(source)))),0u);}
    else if(p.options.w<1.5){s=source_bilinear(uv,0u);}
    else{
        let display=p.layer.zw/p.canvas.xy*vec2<f32>(dims);
        let ratio=vec2<f32>(textureDimensions(source))/max(display,vec2<f32>(0.000001));
        let lod=clamp(log2(max(ratio.x,ratio.y)),0.0,f32(textureNumLevels(source)-1u));
        let low=u32(floor(lod));let high=min(low+1u,textureNumLevels(source)-1u);
        s=source_bicubic(uv,low);
        if(high!=low&&fract(lod)>0.00001){s=mix(s,source_bicubic(uv,high),fract(lod));}
    }
    var opacity=p.options.x;
    if(p.options.z>0.5){
        let m=coordinates(pos,p.mask,p.mask_rotate);
        opacity*=mask_sample(m);
    }
    s*=opacity;
    let b=textureLoad(backdrop,vec2<i32>(id.xy),0);
    let color=blend(b.rgb/max(b.a,0.000001),s.rgb/max(s.a,0.000001),u32(p.options.y));
    let rgb=(1.0-s.a)*b.rgb+(1.0-b.a)*s.rgb+s.a*b.a*color;
    textureStore(destination,vec2<i32>(id.xy),vec4<f32>(rgb,s.a+b.a*(1.0-s.a)));
}
