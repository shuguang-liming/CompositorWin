/* Development-only fixture generator. This executable is never linked into the application.
 * Compile with -I "<OriginalRoot>/Compositor/Rendering" (MSVC: /I) and the original
 * C kernels; use build/c_reference_dither_windows.c for the Windows dither kernel.
 */
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include "BrushPixels.h"
#include "LevelsPixels.h"
#include "NoisePixels.h"
#include "LensPixels.h"
#include "WandPixels.h"
#include "AdjustPixels.h"
#include "HealPixels.h"
#include "ContentFill.h"
#include "DitherPixels.h"

static void emit(const char *name,const uint8_t *p,size_t n,int comma) {
    if(comma)printf(",\n");printf("\"%s\":[",name);
    for(size_t i=0;i<n;++i){if(i)printf(",");printf("%u",p[i]);}printf("]");
}
int main(void){
    uint8_t original[8*8*4],p[sizeof original],out[sizeof original];
    for(size_t i=0;i<64;++i){unsigned a=i%7==0?0:i%3==0?128:255;
        original[i*4]=(uint8_t)(((i*37)%256)*a/255);original[i*4+1]=(uint8_t)(((i*19)%256)*a/255);original[i*4+2]=(uint8_t)(((i*11)%256)*a/255);original[i*4+3]=(uint8_t)a;
    }
    printf("{\n");emit("input",original,sizeof original,0);
    memcpy(p,original,sizeof p);layer_unpremultiply_opaque(p,8*4,8,8);emit("opaque",p,sizeof p,1);
    float tables[768];for(int c=0;c<3;c++)for(int i=0;i<256;i++)tables[c*256+i]=(float)(255-i)/255.0f;
    memcpy(p,original,sizeof p);levels_apply(p,64,tables);emit("levels",p,sizeof p,1);
    memcpy(p,original,sizeof p);noise_add(p,8,8,32,25,0,0,123456);emit("noiseUniform",p,sizeof p,1);
    memcpy(p,original,sizeof p);noise_add(p,8,8,32,25,1,1,123456);emit("noiseGaussian",p,sizeof p,1);
    lens_distort(original,out,8,8,32,-0.35);emit("lens",out,sizeof out,1);
    uint8_t mask[64];wand_mask(original,8,8,32,3,3,1,80,0,mask);emit("wand",mask,64,1);
    uint8_t gradient[768];for(int i=0;i<256;i++){gradient[i*3]=(uint8_t)i;gradient[i*3+1]=(uint8_t)(255-i);gradient[i*3+2]=100;}
    memcpy(p,original,sizeof p);adjust_gradient_map(p,8,8,32,gradient);emit("gradient",p,sizeof p,1);
    float weights[6]={0.4f,0.6f,0.4f,0.6f,0.2f,0.8f};
    memcpy(p,original,sizeof p);adjust_black_white(p,8,8,32,weights,0,40,0.2);emit("blackWhite",p,sizeof p,1);
    float shadows[3]={0.2f,-0.1f,0},midtones[3]={0,0.2f,-0.2f},highlights[3]={-0.1f,0,0.3f};
    memcpy(p,original,sizeof p);adjust_color_balance(p,8,8,32,shadows,midtones,highlights,1);emit("balance",p,sizeof p,1);
    memcpy(p,original,sizeof p);adjust_grain(p,8,8,32,45,2.5,70,123456,0,0,1);emit("grain",p,sizeof p,1);
    memcpy(p,original,sizeof p);adjust_camera_raw(p,8,8,32,1.155,0.97,0.875,0.7,25,-20,30,15,-10,40,-15,0);emit("cameraBasic",p,sizeof p,1);
    memcpy(p,original,sizeof p);adjust_camera_raw_effects(p,8,8,32,50,30,20,35,1,15,20,25,-30,40,20,60,35,0,1);emit("cameraEffects",p,sizeof p,1);
    memcpy(p,original,sizeof p);adjust_camera_raw_calibration(p,8,8,32,30,40,20,-30,25,50,-15,6);emit("cameraCalibration",p,sizeof p,1);
    memcpy(p,original,sizeof p);for(int i=0;i<64;i++)p[i*4+3]=255;emit("inputDetail",p,sizeof p,1);
    adjust_camera_raw_detail(p,8,8,32,70,20,40,15,30,60,20,35,55,60,1);emit("cameraDetail",p,sizeof p,1);
    memcpy(p,original,sizeof p);adjust_camera_raw_optics(p,8,8,32,1,0,100,100,0,35,270,310,25,60,120,30,40,1);emit("cameraOptics",p,sizeof p,1);
    uint8_t heal[24*24*4], coverage[24*24], healed[sizeof heal];
    memset(coverage,0,sizeof coverage);
    for(int y=0;y<24;y++)for(int x=0;x<24;x++){
        int i=y*24+x;heal[i*4]=(x*9+y*3)%256;heal[i*4+1]=(y*8+x*2)%256;heal[i*4+2]=(x*11+y*7)%256;heal[i*4+3]=255;
        if(x>=10&&x<14&&y>=10&&y<14)coverage[i]=(x==10||y==10)?128:255;
    }
    emit("healInput",heal,sizeof heal,1);emit("healMask",coverage,sizeof coverage,1);
    for(int mode=0;mode<3;mode++){char name[16];sprintf(name,"heal%d",mode);memcpy(healed,heal,sizeof heal);spot_heal(healed,coverage,24,24,24*4,0.75f,mode,123456);emit(name,healed,sizeof healed,1);}
    memcpy(healed,heal,sizeof heal);int filled=content_fill(healed,24*4,coverage,24,24,24);if(filled!=1)return 2;emit("contentFill",healed,sizeof healed,1);
    DitherParams dither={0};dither.levels=4;dither.diffusion=0.8f;dither.density=0.2f;dither.contrast=0.15f;dither.cell=4;dither.angle=0.3f;dither.lightOnDark=1;
    dither.dark[0]=10;dither.dark[1]=20;dither.dark[2]=30;dither.light[0]=220;dither.light[1]=230;dither.light[2]=240;dither.dots=0.4f;dither.wobble=3;
    for(int style=0;style<=10;style++){if(style==9)continue;char name[16];sprintf(name,"dither%d",style);dither.style=style;memcpy(healed,heal,sizeof heal);if(!dither_apply(healed,24,24,24*4,&dither))return 3;emit(name,healed,sizeof healed,1);}
    printf("\n}\n");return 0;
}
