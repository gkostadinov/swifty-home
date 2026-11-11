import {$,$$,clamp,reduced} from '../core/util.js';
import {D,T,tick} from '../core/store.js';
import {A} from '../core/catalog.js';
import {SHEET,springEasing} from '../ui/sheets.js';
import {cssv} from '../pages/monitor.js';
import {VOICE} from './voice.js';
import {CHAT,hacking,weather} from './chat.js';
import {WALL} from '../ui/dock.js';

/* ============================================================ Swifty's figure
   The bird from the drawing, in 3D, drawn by this page itself: one WebGL canvas and nothing fetched. It is built
   from a ball, a feather and a wing, lit by one shader, and moved by springs so that it settles like something
   with weight. It draws only while the chat is on screen and the tab is in view, keeps to one still frame per
   change when motion is reduced, and gives way to the flat drawing where WebGL is missing.
   What it does is read from the chat each time the chat is repainted: listening, thinking, talking, pleased,
   sorry, confused, dressed as a hacker over falling code while the owner's agent is running commands, and under
   the weather of its own forecast. The same canvas draws the small house on the lock while nobody is signed in. */
const SWF=(function(){
const {sin,cos,min,max,abs,sqrt,PI}=Math,TAU=PI*2;
const mix=(a,b,k)=>a+(b-a)*k,sat=v=>clamp(v,0,1),seg=(v,a,b)=>sat((v-a)/(b-a)),ease=v=>v*v*(3-2*v);
/* its own small dice, so no two visits move quite alike and the page's one use of chance stays the only one */
let seed=Date.now()%2147483646+1;const dice=(a,b)=>a+(seed=seed*16807%2147483647)/2147483647*(b-a);
const ART='<svg viewBox="-88 -55 1200 1200" focusable="false"><defs><linearGradient id="sw1" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#7DF3FF"/><stop offset=".5" stop-color="#35B2FF"/><stop offset="1" stop-color="#2B5CF2"/></linearGradient><radialGradient id="sw2" cx="50%" cy="38%" r="68%"><stop offset=".62" stop-color="#1532A8" stop-opacity="0"/><stop offset="1" stop-color="#1532A8" stop-opacity=".6"/></radialGradient><linearGradient id="sw3" x1="1" y1=".2" x2="0" y2=".6"><stop offset="0" stop-color="#2C6BF5"/><stop offset="1" stop-color="#1B38B8"/></linearGradient><linearGradient id="sw4" x1="1" y1=".2" x2="0" y2=".7"><stop offset="0" stop-color="#5FD6FF"/><stop offset="1" stop-color="#3478F6"/></linearGradient><linearGradient id="sw5" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#FFFEF9"/><stop offset="1" stop-color="#FFE7C4"/></linearGradient><linearGradient id="sw6" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#FFD76E"/><stop offset="1" stop-color="#FF9338"/></linearGradient><radialGradient id="sw7" cx="50%" cy="88%" r="82%"><stop offset="0" stop-color="#5CE8FF"/><stop offset=".34" stop-color="#2A47C4"/><stop offset=".72" stop-color="#121543"/><stop offset="1" stop-color="#0A0C2A"/></radialGradient><g id="sw8"><path fill="url(#sw3)" d="M304 432C244 470 152 442 70 320 64 420 98 522 160 582 176 616 212 618 222 612 240 650 272 646 284 640 304 672 336 664 352 650L352 470Z"/><path fill="url(#sw4)" d="M312 452C252 494 168 470 112 396 130 472 182 530 250 554 282 566 314 566 340 556Z"/></g><g id="sw9"><circle r="64" fill="url(#sw7)"/><circle cx="-21" cy="-25" r="24" fill="#fff"/><circle cx="23" cy="21" r="10" fill="#fff"/></g><path id="sw0" d="M512 322C486 278 490 232 516 198 542 236 544 282 512 322Z"/></defs><path fill="#2449C8" d="M458 760C448 810 432 850 404 888 452 878 494 846 512 808 530 846 572 878 620 888 592 850 576 810 566 760Z"/><use href="#sw8"/><use href="#sw8" transform="translate(1024 0) scale(-1 1)"/><use href="#sw0" fill="#35B2FF" transform="rotate(-30 512 322) translate(102 66) scale(.8)"/><use href="#sw0" fill="#35B2FF" transform="rotate(30 512 322) translate(102 66) scale(.8)"/><use href="#sw0" fill="#7DF3FF"/><path id="swb" fill="url(#sw1)" d="M512 300C655 300 762 405 762 548 762 695 650 790 512 790 374 790 262 695 262 548 262 405 369 300 512 300Z"/><use href="#swb" fill="url(#sw2)"/><path fill="url(#sw5)" d="M512 428C540 384 608 368 660 402 716 440 724 522 694 596 664 668 592 716 512 720 432 716 360 668 330 596 300 522 308 440 364 402 416 368 484 384 512 428Z"/><ellipse cx="382" cy="620" rx="38" ry="25" fill="#FF7FA8" opacity=".5"/><ellipse cx="642" cy="620" rx="38" ry="25" fill="#FF7FA8" opacity=".5"/><use href="#sw9" x="422" y="528"/><use href="#sw9" x="602" y="528"/><path fill="url(#sw6)" d="M482 576C498 562 526 562 542 576 538 600 526 616 512 622 498 616 486 600 482 576Z"/><path fill="none" stroke="#C8621F" stroke-width="5" stroke-linecap="round" opacity=".75" d="M488 586Q512 600 536 586"/></svg>';

/* ---- small matrix kit: column-major, as WebGL reads it */
const mul=(a,b)=>{const o=new Array(16);for(let c=0;c<4;c++)for(let r=0;r<4;r++)o[c*4+r]=a[r]*b[c*4]+a[4+r]*b[c*4+1]+a[8+r]*b[c*4+2]+a[12+r]*b[c*4+3];return o};
/* move, then turn (yaw over pitch over roll), then size */
function trs(x,y,z,rx,ry,rz,sx,sy,sz){const ca=cos(rx),sa=sin(rx),cb=cos(ry),sb=sin(ry),cc=cos(rz),sc=sin(rz);if(sy==null)sy=sz=sx;
  return [(cb*cc+sb*sa*sc)*sx,ca*sc*sx,(cb*sa*sc-sb*cc)*sx,0,(sb*sa*cc-cb*sc)*sy,ca*cc*sy,(sb*sc+cb*sa*cc)*sy,0,sb*ca*sz,-sa*sz,cb*ca*sz,0,x,y,z,1]}
/* what turns a surface direction the way the matrix turns the surface, mirrored parts included */
function nmat(m){const a=m[0],b=m[1],c=m[2],d=m[4],e=m[5],f=m[6],g=m[8],h=m[9],i=m[10],A=e*i-f*h,B=f*g-d*i,C=d*h-e*g,s=a*A+b*B+c*C<0?-1:1;
  return [A*s,B*s,C*s,(c*h-b*i)*s,(a*i-c*g)*s,(b*g-a*h)*s,(b*f-c*e)*s,(c*d-a*f)*s,(a*e-b*d)*s]}
const hex=h=>[(h>>16)/255,(h>>8&255)/255,(h&255)/255];

/* ---- shapes, each a sheet of points wrapped closed. Nine numbers a point: place, facing, and its own map for the paint */
const vs=[],is=[];
function surf(nu,nv,f,hint){const base=vs.length/9,from=is.length,e=1e-3,d=(a,b)=>[a[0]-b[0],a[1]-b[1],a[2]-b[2]];
  for(let j=0;j<=nv;j++)for(let i=0;i<=nu;i++){const u=i/nu,v=j/nv,p=f(u,v),a=d(f(u+e,v),f(u-e,v)),b=d(f(u,v+e),f(u,v-e)),h=hint(p,u,v);
    let x=a[1]*b[2]-a[2]*b[1],y=a[2]*b[0]-a[0]*b[2],z=a[0]*b[1]-a[1]*b[0],l=Math.hypot(x,y,z);
    if(l<1e-7){x=h[0];y=h[1];z=h[2];l=Math.hypot(x,y,z)||1}else if(x*h[0]+y*h[1]+z*h[2]<0)l=-l;
    vs.push(p[0],p[1],p[2],x/l,y/l,z/l,p[3],p[4],p[5])}
  for(let j=0;j<nv;j++)for(let i=0;i<nu;i++){const a=base+j*(nu+1)+i,b=a+nu+1;is.push(a,b,a+1,a+1,b,b+1)}
  return [from,is.length-from]}
/* the wing's outline, measured off the drawing: s runs from the shoulder to the tip, the front edge sweeps in and the back edge ends in three feathers */
const lead=s=>.24*(1-ease(seg(s,.2,.8)))-.05*sin(PI*seg(s,.5,1.35));
const trail=s=>{let e=-.62*sqrt(max(0,1-sat(s/1.35)**1.6))*(s<.3?sat(.85+.5*s):1);if(s<.5425)e+=.09*(1-sqrt(abs(sin(PI*(.41-s)/.265))));return e};
let BALL,LEAF,WING,HOOP,HAZE,QUAD,BOX,ROOF;
function shapes(){
  BALL=surf(36,22,(u,v)=>{const a=u*TAU,f=sat(v)*PI,x=sin(f)*cos(a),y=cos(f),z=sin(f)*sin(a);return [x,y,z,x,y,z]},p=>p);
  LEAF=surf(12,16,(u,v)=>{v=sat(v);const r=sin(PI*v**.75)**.9,a=u*TAU;return [r*cos(a),v,r*sin(a),r*cos(a),v*2-1,r*sin(a)]},(p,u,v)=>[p[0],v-.4,p[2]]);
  WING=surf(18,60,(u,v)=>{const s=mix(-.3,1.35,sat(v)),a=u*TAU,x=mix(trail(s),lead(s),cos(a)*.5+.5);return [x,s,sin(a)*.075*(1-.5*sat(s/1.35))*sat((1.35-s)*5),x,s,sin(a)]},(p,u)=>[cos(u*TAU)*.4,0,p[5]]);
  const hoop=r=>surf(72,6,(u,v)=>{const a=u*TAU,q=v*TAU,k=1+r*cos(q);return [k*cos(a),r*sin(q),k*sin(a),u,0,0]},(p,u)=>[p[0]-cos(u*TAU),p[1],p[2]-sin(u*TAU)]);
  HOOP=hoop(.014);HAZE=hoop(.05);
  const q=vs.length/9;for(const [x,y] of [[-1,-1],[1,-1],[-1,1],[1,1]])vs.push(x,y,0,0,0,1,x,y,0);QUAD=[is.length,6];is.push(q,q+1,q+2,q+2,q+1,q+3);
  /* flat-sided shapes for the house: each face has its own corners, and so its own facing */
  const solid=faces=>{const from=is.length;for(const f of faces){const b=vs.length/9,[a,c,d]=f,nx=(c[1]-a[1])*(d[2]-a[2])-(c[2]-a[2])*(d[1]-a[1]),ny=(c[2]-a[2])*(d[0]-a[0])-(c[0]-a[0])*(d[2]-a[2]),nz=(c[0]-a[0])*(d[1]-a[1])-(c[1]-a[1])*(d[0]-a[0]),l=Math.hypot(nx,ny,nz);
    for(const p of f)vs.push(p[0],p[1],p[2],nx/l,ny/l,nz/l,p[0],p[1],p[2]);for(let i=2;i<f.length;i++)is.push(b,b+i-1,b+i)}return [from,is.length-from]};
  BOX=solid([[0,1,2],[1,2,0],[2,0,1]].flatMap(([i,j,k])=>[1,-1].map(s=>[[-1,-1],[1,-1],[1,1],[-1,1]].map(([u,v])=>{const p=[0,0,0];p[i]=s;p[j]=u*s;p[k]=v;return p}))));
  ROOF=solid([[[-1,0,1],[0,1,1],[0,1,-1],[-1,0,-1]],[[1,0,-1],[0,1,-1],[0,1,1],[1,0,1]],[[-1,0,1],[1,0,1],[0,1,1]],[[1,0,-1],[-1,0,-1],[0,1,-1]]])}

/* ---- one shader for everything. uK picks the paint: 0 feathers and beak, 1 the body with its face, 2 an eye, 3 a wing, 4 the orbit, 5 a spark, 6 a soft glow,
   7 a lens of the dark glasses, 8 the laptop's lid, 9 the sun, 10 the umbrella, 11 a wall of the house. uH is how far it has dressed as a hacker: the body
   turns to a dark hood and everything is lit green from below. uW is the weather: how much sun, rain and snow */
const VS='attribute vec3 aP,aN,aO;uniform mat4 uM,uV;uniform mat3 uN;uniform vec2 uB;varying vec3 vN,vO,vW;'+
'void main(){vec3 p=aP,n=aN;float y=p.y*p.y,a=uB.x*y,c=cos(a),s=sin(a);'+
'p.yz=vec2(p.y*c-p.z*s,p.y*s+p.z*c);n.yz=vec2(n.y*c-n.z*s,n.y*s+n.z*c);a=uB.y*y;c=cos(a);s=sin(a);'+
'p.xy=vec2(p.x*c-p.y*s,p.x*s+p.y*c);n.xy=vec2(n.x*c-n.y*s,n.x*s+n.y*c);'+
'vec4 w=uM*vec4(p,1.);vW=w.xyz;vN=uN*n;vO=aO;gl_Position=uV*w;}';
const FS='#ifdef GL_FRAGMENT_PRECISION_HIGH\nprecision highp float;\n#else\nprecision mediump float;\n#endif\n'+
'varying vec3 vN,vO,vW;uniform float uK,uX,uH;uniform vec3 uA,uZ,uS,uG,uW;uniform vec4 uE,uF,uC;'+
/* soft key light from the upper left, a cool shade instead of black, a cyan edge from behind, one glint */
'vec3 lit(vec3 b,vec3 sh,vec3 n,float g){float d=dot(n,vec3(-.42,.72,.55))*.5+.5,f=1.-max(n.z,0.);'+
/* snow lies on whatever faces up, and the sun warms it from above */
'b=mix(b,vec3(.93,.97,1.),uW.z*smoothstep(.42,.85,n.y));'+
'vec3 c=mix(b*sh,b,smoothstep(.18,.82,d))+b*.1*max(n.y,0.)+vec3(1.,.74,.32)*max(n.y,0.)*.26*uW.x;'+
'c+=vec3(.45,.82,1.)*f*f*f*.85*smoothstep(-.3,.5,dot(n,vec3(.55,.45,-.7)))+vec3(.1,.85,.42)*max(-n.y,0.)*(.6+.4*n.z)*.34*uH;'+
'return c+g*pow(max(dot(n,vec3(-.24,.41,.88)),0.),40.);}'+
/* parts darken a little where they meet the body */
'float meet(float k){return mix(1.,smoothstep(uC.w*.8,uC.w*1.5,distance(vW,uC.xyz))*.5+.5,k);}'+
'void main(){vec3 n=normalize(vN),c;float x=uX,a;vec4 o;'+
'if(uK<.5){c=lit(mix(uZ,uA,smoothstep(-1.,1.,vO.y)),uS,n,uF.x)*meet(uF.y)+uA*uF.z;o=vec4(c,1.);}'+
'else if(uK<1.5){vec2 q=vO.xy;float t=vO.y*.5+.5;'+
'vec3 b=mix(vec3(.169,.361,.949),vec3(.208,.698,1.),smoothstep(0.,.55,t));b=mix(b,vec3(.49,.953,1.),smoothstep(.5,1.,t));'+
/* the heart-shaped face: two lobes and a wide chin, run together */
'float l=length(vec2(abs(q.x)-.36,q.y-.15))-.47,w=(length((q+vec2(0.,.06))/vec2(.77,.64))-1.)*.64,k=clamp(.5+5.*(w-l),0.,1.),d=mix(w,l,k)-.1*k*(1.-k);'+
'float m=(1.-smoothstep(-x,x,d))*step(0.,vO.z);'+
'vec3 fc=mix(vec3(1.,.906,.769),vec3(1.,.996,.976),smoothstep(-.7,.6,q.y));'+
'fc=mix(fc,vec3(1.,.5,.66),uG.z*.8*(1.-smoothstep(.3,1.25,length(vec2(abs(q.x)-.52,q.y+.3)/vec2(.17,.115)))));'+
'fc*=.84+.16*smoothstep(.27,.43,length(vec2(abs(q.x)-.36,q.y-.07)));fc*=.87+.13*smoothstep(.04,.2,length(vec2(q.x,(q.y+.27)*1.3)));'+
'b=mix(b,mix(vec3(.045,.075,.06),vec3(.11,.17,.14),t),uH);b=mix(b,vec3(.2,.33,.26),uH*smoothstep(0.,.025,d)*(1.-smoothstep(.07,.11,d)));'+
'float f=1.-max(n.z,0.);b=mix(b,mix(vec3(.082,.196,.66),vec3(.02,.05,.035),uH),.4*f*f);'+
'c=mix(lit(b,vec3(.4,.47,.8),n,.3)+(.2-.14*uH)*pow(max(dot(n,vec3(-.5,.7,.51)),0.),5.),lit(fc,vec3(.9,.82,.86),n,.1),m);o=vec4(c,1.);}'+
'else if(uK<2.5){vec2 e=vO.xy;float g=length(e-vec2(uG.x*.3,uG.y*.3-.76))/1.64,f=1.-max(n.z,0.);'+
'c=mix(vec3(.36,.91,1.),vec3(.165,.28,.77),smoothstep(0.,.34,g));c=mix(c,vec3(.07,.082,.263),smoothstep(.34,.72,g));c=mix(c,vec3(.04,.047,.165),smoothstep(.72,1.,g));'+
'c+=vec3(.2,.4,.75)*f*f*.5;vec2 h=e-uG.xy*.1;'+
'c=mix(c,vec3(1.),(1.-uE.y)*(1.-smoothstep(.36-x,.36+x,length(h-vec2(-.33,.39)))));c=mix(c,vec3(1.),.92*(1.-uE.y)*(1.-smoothstep(.15-x,.15+x,length(h-vec2(.36,-.33)))));'+
/* lids: the upper one comes down to blink and slopes when sorry; a smile closes both to one upturned arc */
'float arc=.1-.55*e.x*e.x,up=mix(mix(-.5,1.3,uE.x)+.25*e.x*e.x-uE.z*(.45+.5*uE.w*e.x),arc+.28-.25*e.x*e.x,uE.y),lo=mix(mix(-.55+.2*e.x*e.x,-1.5,uE.x),arc,uE.y),dl=min(up-e.y,e.y-lo);'+
'vec3 sk=lit(vec3(1.,.95,.87),vec3(.9,.82,.86),n,.1);'+
'sk=mix(sk,vec3(.2,.16,.36),.75*(1.-smoothstep(.03,.1+x,abs(e.y-up)))*max(1.-uE.x*uE.x,uE.z));'+
'c*=mix(.55,1.,smoothstep(0.,.3,dl));c=mix(sk,c,smoothstep(-x,x,dl));o=vec4(c,1.);}'+
'else if(uK<3.5){float s=vO.y,h=vO.x,le=.24*(1.-smoothstep(.2,.8,s))-.05*sin(3.1416*clamp((s-.5)/.85,0.,1.)),lw=mix(-.29,-.12,smoothstep(.5,1.03,s));'+
'c=mix(vec3(.173,.42,.96),vec3(.106,.22,.72),clamp(s/1.35,0.,1.));'+
'c=mix(c,mix(vec3(.373,.84,1.),vec3(.204,.47,.965),clamp(s,0.,1.)),smoothstep(-x,x,h-lw)*smoothstep(-x,x,le-.07-h)*(1.-smoothstep(1.,1.03,s)));'+
'c=mix(c,vec3(.435,.706,1.),.5*(1.-smoothstep(.01,.016+x,abs(fract((.41-s)/.265+.5)-.5)*.265))*step(h,lw)*step(s,.5)*step(-.2,s));'+
'c=lit(c,vec3(.45,.5,.84),n,.2)*meet(1.);o=vec4(c,1.);}'+
'else if(uK<4.5){float d=fract(uF.x-vO.x);a=mix(pow(1.-d,3.),1.,uF.z)*uF.y;o=vec4(mix(uZ,uA,pow(1.-d,8.))*a,a*uF.w);}'+
'else if(uK<5.5){vec2 p=abs(vO.xy)*1.7;float k=1.-smoothstep(1.-x*3.,1.,sqrt(p.x)+sqrt(p.y)),g=exp(-dot(p,p)*1.5)*.5;'+
'o=vec4(mix(uZ,uA,vO.y*.85+.5)*k+uZ*g,k+g*uF.w)*uF.y;}'+
'else if(uK<6.5){a=exp(-dot(vO.xy,vO.xy)*4.5)*uF.y;o=vec4(uA*a,a*uF.w);}'+
/* black glass with green code running down it */
'else if(uK<7.5){float r=fract(sin(floor(vO.x*4.+4.)*91.7)*437.5),y=fract(vO.y*.6+uF.x*(.25+r*.5)+r),f=1.-max(n.z,0.);'+
'c=vec3(.02,.035,.03)+vec3(.1,.9,.45)*smoothstep(.72,1.,y)*.6*step(.3,r)+vec3(.5,.9,.7)*f*f*.35+.5*pow(max(dot(n,vec3(-.24,.41,.88)),0.),30.);o=vec4(c,1.);}'+
/* a rounded slab of dark metal with a lit mark in the middle */
'else if(uK<8.5){vec2 p=abs(vO.xy)*vec2(.8,.5)-vec2(.7,.4);float d=length(max(p,0.))-.1,g=length(vO.xy*vec2(.8,.5));a=1.-smoothstep(-x*2.,0.,d);'+
'c=mix(vec3(.1,.12,.13),vec3(.21,.24,.25),vO.y*.5+.5)+uA*(1.-smoothstep(.07,.1,g))+uA*exp(-g*g*14.)*.4;o=vec4(c*a,a);}'+
/* the sun: a warm disc with short rays that turn slowly */
'else if(uK<9.5){float r=length(vO.xy),k=abs(fract((atan(vO.y,vO.x)+uF.x)*1.5915)-.5),core=1.-smoothstep(.4-x*2.,.4,r),ray=(1.-smoothstep(.12,.2,k))*smoothstep(.5,.56,r)*(1.-smoothstep(.8,.9,r));'+
'o=vec4(mix(vec3(1.,.74,.22),vec3(1.,.95,.6),clamp(1.-r*2.2,0.,1.))*(core+ray)+vec3(1.,.7,.25)*exp(-r*r*3.5)*.4,core+ray)*uF.y;}'+
/* the umbrella: the top half of a ball in eight panels, scalloped at the rim, darker underneath */
'else if(uK<10.5){float k=cos(atan(vO.z,vO.x)*4.);if(vO.y<.16*abs(k))discard;'+
'c=lit(mix(uZ,uA,step(0.,k))*(gl_FrontFacing?1.:.6),vec3(.8,.62,.45),gl_FrontFacing?n:-n,.25);o=vec4(c,1.);}'+
/* a wall of the house: two windows a side, lit from inside by uF.x, and a door at the front */
'else{vec2 q=abs(vO.z)>.99?vO.xy:vO.zy;float w=(1.-smoothstep(0.,.04,max(abs(abs(q.x)-.5)-.22,abs(q.y-.12)-.36)))*step(abs(vO.y),.99),d=step(.99,vO.z)*(1.-smoothstep(0.,.04,max(abs(q.x)-.2,abs(q.y+.48)-.52)));'+
'c=mix(mix(lit(uA,uS,n,.1),uZ,d),vec3(1.,.84,.46)*(.28+1.3*uF.x),w*(1.-d));o=vec4(c,1.);}'+
'gl_FragColor=o;}';

let host=null,fig=null,cv=null,flat=null,gl=null,U=null,ok=false,lost=false,dark=true;
function build(){
  try{gl=cv.getContext('webgl',{alpha:true,antialias:true,powerPreference:'low-power'})}catch(e){gl=null}
  if(!gl)return false;
  if(!vs.length)shapes();
  const sh=(t,s)=>{const o=gl.createShader(t);gl.shaderSource(o,s);gl.compileShader(o);return o};
  const pr=gl.createProgram();gl.attachShader(pr,sh(gl.VERTEX_SHADER,VS));gl.attachShader(pr,sh(gl.FRAGMENT_SHADER,FS));
  ['aP','aN','aO'].forEach((n,i)=>gl.bindAttribLocation(pr,i,n));gl.linkProgram(pr);
  if(!gl.getProgramParameter(pr,gl.LINK_STATUS))return false;
  gl.useProgram(pr);U={};for(const n of ['uM','uV','uN','uB','uK','uX','uH','uA','uZ','uS','uG','uW','uE','uF','uC'])U[n]=gl.getUniformLocation(pr,n);
  gl.bindBuffer(gl.ARRAY_BUFFER,gl.createBuffer());gl.bufferData(gl.ARRAY_BUFFER,new Float32Array(vs),gl.STATIC_DRAW);
  gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER,gl.createBuffer());gl.bufferData(gl.ELEMENT_ARRAY_BUFFER,new Uint16Array(is),gl.STATIC_DRAW);
  for(let i=0;i<3;i++){gl.enableVertexAttribArray(i);gl.vertexAttribPointer(i,3,gl.FLOAT,false,36,i*12)}
  gl.blendFunc(gl.ONE,gl.ONE_MINUS_SRC_ALPHA);gl.clearColor(0,0,0,0);
  /* a long lens: the figure fills 4.8 body sizes across, barely foreshortened */
  const t=14/2.4;gl.uniformMatrix4fv(U.uV,false,mul([t,0,0,0,0,t,0,0,0,0,-28/16,-1,0,0,-2*22*6/16,0],trs(0,0,-14,0,0,0,1)));
  return true}

/* ---- the pose: where each joint is (P), how fast it moves (V) and where it is heading (T). K is each spring's pull and how soon it settles */
const K={x:[70,.62],y:[70,.55],yaw:[90,.7],pit:[90,.7],rol:[90,.5],sq:[260,.42],wl:[130,.55],wr:[130,.55],ws:[60,.8],wc:[110,.4],wt:[90,.6],tf:[150,.35],tw:[120,.2],ts:[90,.28],
  eo:[420,1],eh:[260,.8],es:[160,1],eq:[200,.7],ew:[200,.5],bk:[900,.8],lx:[240,.8],ly:[240,.8],bl:[30,1],ro:[30,1],gl:[40,1],op:[26,1],fa:[40,1],ff:[30,1],hk:[55,.45],su:[45,.5],rn:[45,.5],sn:[45,.6]};
const P={},V={},T={};for(const k in K)P[k]=V[k]=T[k]=0;P.eo=1;
let clock=0,since=0,st='idle',talk=0,shot=null,spin=0,ph=0,ring=1,blink=0,nextBlink=2,gaze=[0,0],nextGaze=1,fid=null,nextFid=6,lvl=0,sp=[1,1,0],bits=[];
let hero=null,cur=[0,0,1],nest=[0,0],vis=null,fl=null,vx=0,vy=0,seen=0,liveMsg=null,first=false,raf=0,prev=0,lastT='';
let hack=false,rain=null,pen=null,drops=[],rainAt=0,soaked=false;
/* the weather it stands in, and what falls. UX and UY are where the umbrella is held */
let sky=null,fall=[],wet=[.62,.8,1],flake=[[1,1,1],[.82,.9,1]],tint=[[.48,.36,.96],[.4,.27,.87]];
const UX=.42,UY=1.14,drip=(snow,any)=>({x:dice(-2.3,2.3),y:any?dice(-2.4,2.6):dice(2.5,3.1),v:snow?dice(.45,.9):dice(4.2,6),p:dice(0,TAU),s:snow});
/* the lock's scene: whether a passkey is being checked, and when it was found good */
let scene='bird',lockEl=null,busy=false,hi=-1,popped=false,hc=0;
const LEN={hello:1.3,send:.55,pleased:2,sorry:3,confused:2.4,poke:.95},HOLD={hello:.5,send:.3,pleased:1.2,sorry:1.2,confused:.9,poke:.5};
function shoot(n){if(!LEN[n])return;shot={n,t:clock,d:LEN[n],f:0};wake();if(reduced||!ok){still();setTimeout(()=>{if(shot&&shot.n===n){shot=null;still()}},LEN[n]*1000)}}
function burst(n){for(let i=0;i<n;i++)bits.push({x:P.x+dice(-.5,.5),y:P.y+dice(0,.7),vx:dice(-2.2,2.2),vy:dice(.8,2.8),t:0,d:dice(.55,1.1),s:dice(.1,.24),r:dice(0,3)})}
/* how loud the held microphone is right now, from the samples the page is already keeping */
function level(){const r=VOICE.rec,c=r&&r.chunks[r.chunks.length-1];let v=0;if(c){for(let i=0;i<c.length;i+=8)v+=c[i]*c[i];v=min(1,sqrt(v*8/c.length)*4)}return lvl=mix(lvl,v,.3)}

function aim(){
  const o=P.op,u=shot?(reduced?HOLD[shot.n]:clock-shot.t):0,s=reduced?1:clock-since,now=st==='idle'&&!reduced&&clock<talk?'talk':st;
  /* at rest the wings are spread like the drawing where there is room, and swept back in the margin */
  let x=0,y=0,yaw=gaze[0]*.25,pit=-gaze[1]*.12,rol=0,sq=0,wl=mix(-.2,.02,o),wr=wl,ws=mix(.85,.1,o),wc=0,wt=0,tf=0,eo=1,eh=0,es=0,eq=0,ew=0,bk=0,lx=gaze[0],ly=gaze[1],bl=.7,ro=o*.55,gl=.4,fa=mix(.06,.09,o),ff=8;
  spin=0;
  if(now==='listen'){const lv=level();yaw=.4;rol=-.13;x=.08;pit=.05;ew=1;tf=1;wl=wr=mix(-.05,.12,o);ws=mix(.45,.1,o);fa=.025;ff=24;ro=.45+lv*.55;sq=lv*.07;gl=.5+lv*.5;lx=.55;ly=-.15;bl=.8}
  else if(now==='think'){yaw=-.28+.1*sin(s*.8);pit=-.2+.04*sin(s*1.7);rol=.16*sin(s*.65+.5);y=.03;ro=1;fa=.045;ff=6;tf=.35;gl=.55+.15*sin(s*3);
    lx=clamp(sp[0]*.5,-.75,.75);ly=clamp(.25+sp[1]*.5,-.3,.8)}
  else if(now==='talk'){const e=reduced?0:sat(sin(s*13+1.7*sin(s*4.1))*.9+.2)*(.55+.45*sin(s*2.7+1));
    bk=e;pit=.04+e*.06;yaw=.22+.05*sin(s*2.1);rol=.05*sin(s*3.3);y=e*.02;tf=.25*e;lx=.4;ly=0;eh=.15;wl+=.14*max(0,sin(s*2.3));wr+=.14*max(0,sin(s*2.3+2.4));ws=mix(.6,.1,o);gl=.5}
  else if(fid){const f=clock-fid.t;if(f>1.2)fid=null;else if(fid.k<1){fa=.36;ff=26;y=.06}else if(fid.k<2)rol=.22*fid.s;else if(fid.k<3){lx=.6;ly=-.5;yaw=.2;pit=.08}else if(!fid.f){fid.f=1;V.tw+=8*fid.s;blink=.001}}
  /* as a hacker it keeps its laptop: no orbit, and while the commands run its wings are at the keys */
  if(hack){ro=0;gl=.55;if(now==='think'){yaw=.08*sin(s*1.1);pit=.13+.02*sin(s*7);rol=0;y=-.02;wl=-.95+.07*sin(clock*31);wr=-.95+.07*sin(clock*27+1);ws=-.55;fa=.01;tf=.2;lx=.2*sin(s*2.3);ly=-.5}}
  /* under the weather of its own answer: an umbrella held up in the rain, a shiver and tucked wings in the snow, wings out to the sun */
  if(sky&&(now==='idle'||now==='talk')){ro=0;
    if(sky==='rain'){wr=.3;wt=.25;ws=mix(ws,.25,.6);tf=-.25;y-=.02;ly=.25+.2*sin(s*.7);gl=.3}
    else if(sky==='snow'){x+=.012*sin(clock*41)*(sin(clock*1.7)>.2?1:0);wl=wr=-.4;ws=mix(.95,.55,o);bl=1;tf=.1;gl=.5}
    else{wl+=.14;wr+=.14;ws=mix(ws,.2,.5);bl=.9;rol+=.05*sin(s*1.4);ly=.2;gl=.75}}
  if(fl){const k=sat(fl.t/fl.d);fa=.55;ff=30;ws=.08;wl=wr=0;rol=clamp(-vx*.22,-.5,.5)*(1-k*k);yaw=clamp(vx*.3,-.7,.7)*(1-k);pit=clamp(-vy*.12,-.3,.3);tf=.4;ew=.3;eh=0}
  if(shot){const n=shot.n,once=(b,f)=>{if(!(shot.f&b)){shot.f|=b;if(!reduced)f()}};
    if(n==='hello'){eh=seg(u,.1,.25)*(1-seg(u,.95,1.2));if(u>.15&&u<.95){wr=.3+.38*sin(u*24);wt=.2}ws=.05;rol=.1;yaw=.12;bl=1;bk=u>.12&&u<.4?.6:0;gl=.7;tf=.6}
    else if(n==='send'){once(1,()=>{V.y+=1.1});lx=.8;ly=.3;yaw=.34;pit=-.08;tf=.8;ew=.6}
    else if(n==='pleased'){const k=seg(u,.18,.66);
      if(u<.18){sq=-.14;y=-.1;wl=wr=-.5;eo=.8}
      else{once(1,()=>{V.sq+=2.4});y=.7*sin(PI*k);spin=TAU*ease(k);wl=wr=k<1?.42:.15;ws=.02;fa=k<1?.2:.1;ff=32;eh=u<1.6?1:0;bl=1;gl=1;tf=.8;
        if(k>.45)once(2,()=>burst(9));if(k>=1){once(4,()=>{V.sq-=2.6;V.y-=.8});rol=.1*sin((u-.66)*15)*max(0,1-(u-.66)/.8);spin=0}}}
    else if(n==='sorry'){const w=seg(u,0,.35)*(1-seg(u,shot.d-.6,shot.d));
      y=-.13*w;pit=.33*w;tf=-w;wl=wr=mix(wl,-1.15,w);wc=.4*w;ws=mix(ws,.3,w);es=w;eo=1-.22*w;ly=-.6*w;lx=0;bl=.3;gl=.15;fa=.02;
      yaw=.2*sin((u-.6)*6.5)*seg(u,.6,.8)*(1-seg(u,1.9,2.2));bk=u>.3&&u<.75?.3:0}
    else if(n==='confused'){const w=1-seg(u,shot.d-.5,shot.d),side=u<1.25?1:-1;
      rol=(u<1.25?.42:-.3)*w;yaw=-.12*side*w;eq=.5*side*w;ew=.5*w;once(1,()=>{V.tf+=9;V.tw+=5});wl+=.12*w;wr-=.18*w;lx=.45*sin(u*5.5)*w;ly=.15*w;tf=.4*w;
      if(abs(u-.5)<.07||abs(u-.78)<.07)eo=0}
    else if(n==='poke'){if(u<.13){sq=-.3;eo=0}else{once(1,()=>{V.y+=1.7;V.sq+=2.5;burst(4)});eo=u<.3?0:1;eh=u>.3?1-seg(u,.7,.9):0;tf=1;rol=.14*sin(u*20)*max(0,1-u);bl=1}}
    if(!reduced&&u>shot.d)shot=null}
  Object.assign(T,{x,y,yaw,pit,rol,sq,wl,wr,ws,wc,wt,tf,eo,eh,es,eq,ew,bk,lx,ly,bl,ro,gl,fa,ff,op:hero?1:0,hk:hack?1:0,su:sky==='sun'?1:0,rn:sky==='rain'?1:0,sn:sky==='snow'?1:0});
  /* what trails behind: the tuft and the tail lag the body */
  T.tw=-V.x*.5-V.yaw*.25-V.rol*.4;T.ts=V.x*.4+V.yaw*.2+.07*sin(clock*1.3)}

/* where the figure stands: over the empty chat's stage, or at home in the margin */
function spot(){const d=fig.offsetWidth;nest=[host.offsetLeft+fig.offsetLeft+d/2,host.offsetTop+fig.offsetTop+d/2];
  return hero&&hero.isConnected?[hero.offsetLeft+hero.offsetWidth/2-nest[0],hero.offsetTop+hero.offsetHeight/2-nest[1],min(hero.offsetHeight*1.5,hero.offsetWidth+48)/d]:[0,0,1]}
function stand(c,tg){cur=c;vis=[nest[0]+c[0],nest[1]+c[1]-host.parentNode.scrollTop];
  const t=`translate(${c[0].toFixed(1)}px,${c[1].toFixed(1)}px) scale(${c[2].toFixed(3)})`;if(t!==lastT)fig.style.transform=lastT=t;
  if(!ok)return;const want=min(1024,Math.round(fig.offsetWidth*min(devicePixelRatio||1,2)*(fl?max(fl.a[2],tg[2]):c[2])));if(abs(cv.width-want)>1)cv.width=cv.height=want}
function land(f){V.sq-=2.2;V.y-=1.3;if(f.hello){shoot('hello');burst(8)}}
/* the falling code behind the chat while it is at a shell: a plain 2D canvas behind the whole sheet, one new row of characters a step.
   It is as tall as the window, so the sheet can grow over it without the rain starting again */
function pour(){const sb=host.parentNode,w=SHEET.el.clientWidth,h=innerHeight,ch=()=>String.fromCharCode(dice(0,1)<.3?48+dice(0,10):0xFF66+dice(0,56));
  if(!rain){rain=document.createElement('canvas');rain.className='rain';rain.setAttribute('aria-hidden','true');pen=rain.getContext('2d')}
  if(rain.parentNode!==sb)sb.appendChild(rain);
  if(rain.width!==w||rain.height!==h){rain.width=w;rain.height=h;drops=[];soaked=false}
  pen.globalCompositeOperation='destination-out';pen.fillStyle='rgba(0,0,0,.09)';pen.fillRect(0,0,w,h);
  pen.globalCompositeOperation='source-over';pen.font='13px ui-monospace,Menlo,Consolas,monospace';
  for(let i=0;i*14<w;i++){const d=drops[i]||(drops[i]={y:-dice(0,h/16)|0,c:ch()});
    pen.clearRect(i*14,d.y*16-13,14,16);pen.fillStyle='rgba(34,197,94,.6)';pen.fillText(d.c,i*14+1,d.y*16);
    d.c=ch();d.y++;pen.fillStyle='rgba(200,255,220,.9)';pen.fillText(d.c,i*14+1,d.y*16);
    if(d.y*16>h&&dice(0,1)>.94)d.y=-dice(0,12)|0}}

function step(dt){
  clock+=dt;ph+=P.ff*dt;ring+=dt*(st==='think'?3.6:st==='listen'?2:.7);
  const tg=spot();let c=tg;
  if(fl){fl.t+=dt;const k=sat(fl.t/fl.d),e=1-(1-k)**3;c=[mix(fl.a[0],tg[0],e),mix(fl.a[1],tg[1],e)+fl.dip*sin(PI*k)*(1-k),mix(fl.a[2],tg[2],e)]}
  const r=fig.offsetWidth/4.8*c[2];vx=mix(vx,(c[0]-cur[0])/dt/r,.3);vy=mix(vy,(cur[1]-c[1])/dt/r,.3);
  if(fl&&fl.t>=fl.d){const f=fl;fl=null;land(f)}
  stand(c,tg);
  if(blink){blink+=dt/.15;if(blink>=1)blink=0}else if(clock>nextBlink){blink=.001;nextBlink=clock+(dice(0,1)<.2?.28:dice(2.2,5.5))}
  if(clock>nextGaze){gaze=dice(0,1)<.45?[0,0]:[dice(-.5,.5),dice(-.2,.3)];nextGaze=clock+dice(.8,2.8)}
  if(st==='idle'&&clock>nextFid){fid={k:dice(0,4),s:dice(0,1)<.5?-1:1,t:clock,f:0};nextFid=clock+dice(5,10)}
  if(hack&&clock>rainAt){rainAt=clock+(CHAT.busy?.075:.13);pour()}
  /* rain and snow fall around it; over the umbrella the rain stops on the cloth with a small splash */
  if(sky==='rain'||sky==='snow'){const snow=sky==='snow';while(fall.length<(snow?16:22))fall.push(drip(snow,1));
    for(const d of fall){d.y-=d.v*dt;if(d.s)d.x+=.35*sin(clock*1.6+d.p)*dt;
      const roof=!d.s&&abs(d.x-P.x-UX)<1.2?P.y+UY+.6*sqrt(1-((d.x-P.x-UX)/1.3)**2):-2.6;
      if(d.y<roof){if(roof>-2&&dice(0,1)<.5)bits.push({x:d.x,y:roof,vx:dice(-.7,.7),vy:dice(.4,.9),t:0,d:.25,s:.06,r:0});Object.assign(d,drip(snow,0))}}}
  else if(fall.length&&P.rn<.05&&P.sn<.05)fall=[];
  aim();
  for(let i=0;i<2;i++)for(const k in K){const c=K[k];V[k]+=((T[k]-P[k])*c[0]-2*c[1]*sqrt(c[0])*V[k])*dt/2;P[k]+=V[k]*dt/2}
  bits=bits.filter(b=>{b.t+=dt;b.x+=b.vx*dt;b.y+=b.vy*dt;b.vy-=3.5*dt;return b.t<b.d})}

function put(mesh,m,o){
  gl.uniformMatrix4fv(U.uM,false,m);gl.uniformMatrix3fv(U.uN,false,nmat(m));gl.uniform1f(U.uK,o.k||0);gl.uniform1f(U.uX,(o.x||1)*4.8/cv.width);
  gl.uniform3fv(U.uA,o.a||ONE);gl.uniform3fv(U.uZ,o.z||o.a||ONE);gl.uniform3fv(U.uS,o.s||BLUE);gl.uniform4fv(U.uF,o.f||NIL);gl.uniform2fv(U.uB,o.b||FLAT);
  gl.drawElements(gl.TRIANGLES,mesh[1],gl.UNSIGNED_SHORT,mesh[0]*2)}
const ONE=[1,1,1],NIL=[0,0,0,1],FLAT=[0,0],BLUE=[.45,.5,.84],WARM=[.9,.6,.5],MIRROR=[-1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1];
/* the drawing's own colours, tip first then root */
const TAIL=[hex(0x1A2F9E),hex(0x2C6BF5)],TUFT=[hex(0x45BDFF),hex(0x2E9BF5)],CREST=[hex(0x9AF8FF),hex(0x4CC8FF)],BEAK=[hex(0xFF9338),hex(0xFFD76E)],CHIN=[hex(0xD9661F),hex(0xF0862E)],MOUTH=[hex(0x8A2A3A),hex(0x5A1524)],
  GOLD=[hex(0xFFFBE0),hex(0xFFCF4D)],NIGHT=[hex(0xFFF4C2),hex(0x8BE9FF),[.3,.55,1],[1,1,1],hex(0xBFE9FF)],LIME=[.1,.85,.4],LENS=[.03,.05,.04],GLOW=[.2,1,.55],
  ICE=[.82,.9,1],SCARF=hex(0xE5484D),BROLLY=[hex(0xFFC43A),hex(0xFF9F1C)],STICK=[.25,.27,.38],SUNNY=[1,.72,.3],WALL=[.96,.94,.9],SHADOW=[.72,.74,.9],LAMP=[1,.86,.42],METAL=[.62,.65,.72],DARK=[.13,.14,.18];
let ink=NIGHT;
function draw(){
  const w=cv.width;gl.viewport(0,0,w,w);gl.depthMask(true);gl.clear(gl.COLOR_BUFFER_BIT|gl.DEPTH_BUFFER_BIT);
  if(scene==='home')return house();
  const mv=reduced?0:1,flap=P.fa*sin(ph)*mv,by=P.y+(.045*sin(clock*1.9)-P.fa*.1*cos(ph))*mv,bx=P.x+.02*sin(clock*1.1)*mv,q=1+P.sq+.012*sin(clock*2.6)*mv;
  const R=trs(bx,by,0,P.pit,P.yaw+spin,P.rol,1/sqrt(q),q,1/sqrt(q)),B=m=>mul(R,m),bl=sin(PI*blink);
  const hk=sat(P.hk),gear=P.hk>.02,drop=(1-P.hk)*1.4;
  gl.uniform4f(U.uC,bx,by,0,1);gl.uniform3f(U.uG,P.lx,P.ly,P.bl);gl.uniform1f(U.uH,hk);gl.uniform3f(U.uW,sat(P.su),sat(P.rn),sat(P.sn));
  /* the glow it floats in */
  gl.disable(gl.DEPTH_TEST);gl.enable(gl.BLEND);
  put(QUAD,trs(bx*.7,by*.7+.05,-2,0,0,0,2.6),{k:6,a:hk>.5?LIME:P.su>.5?SUNNY:ink[2],f:[0,P.gl*(dark||hk>.5?.5:.28),0,dark||hk>.5?.15:1]});
  gl.enable(gl.DEPTH_TEST);gl.disable(gl.BLEND);
  put(BALL,B(trs(0,0,0,0,0,0,1,.98,.95)),{k:1});
  const tf=P.tf,low=max(0,-tf);
  for(const s of [-1,1]){
    /* an eye, set into the face and turned to look out along it */
    const open=sat(P.eo-bl-max(0,s*P.eq)),wide=.27*(1+.1*P.ew);
    gl.uniform4f(U.uE,open,P.eh,P.es,s);
    put(BALL,B(trs(s*.324,.063,.795,-.07,s*.369,0,wide,wide*(1+.06*P.ew),.15)),{k:2,x:1/wide});
    /* a wing: lifted at the shoulder, swept back, twisted, bent along its length by its own beat, and drooping when it is sorry */
    const W=trs(-.7,.05,-.2,P.wt,-P.ws,.89-(s<0?P.wl:P.wr)-flap,1);
    put(WING,B(s<0?W:mul(MIRROR,W)),{k:3,b:[-P.fa*1.3*sin(ph-1.1)*mv,P.wc]});
    put(LEAF,B(trs(s*.1,-.8,-.22,.25,0,PI+s*.52+P.ts*.5,.16,.7,.05)),{a:TAIL[0],z:TAIL[1],f:[.15,1,0,1],b:[0,P.ts*.8]});
    put(LEAF,B(trs(s*.05,.88,-.06,-.1+low*.5-tf*.1,0,-s*(.5+tf*.18)+P.tw*.4,.085,.44*(1-low*.2),.045)),{a:TUFT[0],z:TUFT[1],f:[.2,.6,0,1],b:[0,P.tw*1.1]});
    /* dark glasses, which drop onto the beak from above */
    if(gear)put(BALL,B(trs(s*.372,.075+drop,.9,-.07,s*.369,-s*.1,.31,.235,.07)),{k:7,f:[clock,0,0,1]})}
  if(gear)put(BALL,B(trs(0,.11+drop,.985,0,0,0,.1,.035,.04)),{a:LENS,z:LENS,f:[.4,0,0,1]});
  put(LEAF,B(trs(0,.9,-.03,-.12+low*.5-tf*.12,0,P.tw*.5,.1,.56*(1+tf*.1),.05)),{a:CREST[0],z:CREST[1],f:[.25,.6,0,1],b:[0,P.tw*1.2]});
  /* the beak opens on a hinge, with the dark of the mouth behind it */
  put(LEAF,B(trs(0,-.15,.84,PI/2+.42-P.bk*.2,0,0,.165,.36,.095)),{a:BEAK[0],z:BEAK[1],s:WARM,f:[.55,0,0,1]});
  put(LEAF,B(trs(0,-.21,.82,PI/2+.85+P.bk*.8,0,0,.12,.27,.065)),{a:CHIN[0],z:CHIN[1],s:WARM,f:[.2,0,0,1]});
  if(P.bk>.05)put(BALL,B(trs(0,-.21,.9,0,0,0,.085,.02+.06*P.bk,.05)),{a:MOUTH[0],z:MOUTH[1],s:WARM});
  /* winter things: a cap of snow, and a red scarf with one end hanging */
  if(P.sn>.02){const k=sat(P.sn);put(BALL,B(trs(0,.84,-.02,0,0,0,.62*k,.26*k,.58*k)),{a:ONE,z:ICE,f:[.1,0,0,1]});
    put(HAZE,B(trs(0,-.47,0,0,0,0,.9,2.7*k,.86)),{a:SCARF,z:SCARF,s:WARM,f:[.1,0,0,1]});
    put(LEAF,B(trs(.4,-.5,.74,.12,0,PI-.22+P.ts*.7,.11,.44*k,.035)),{a:SCARF,z:SCARF,s:WARM,f:[.1,0,0,1],b:[0,P.ts]})}
  /* the umbrella: its cloth over the head, and its stick down to the wing that holds it */
  if(P.rn>.02){const k=max(.01,P.rn);put(BALL,B(trs(UX,UY,0,0,0,.14,1.28*k,.6*k,1.28*k)),{k:10,a:BROLLY[0],z:BROLLY[1]});
    put(LEAF,B(trs(UX+.36,-.34,.78,0,0,.2,.03,1.72*k,.03)),{a:STICK,z:STICK,f:[.5,0,0,1]})}
  /* the orbit and its spark, behind the body where they pass behind it */
  gl.enable(gl.BLEND);gl.depthMask(false);
  if(P.su>.02){const z=.8*max(.01,P.su);put(QUAD,trs(bx*.3+1.2,by*.3+1.5,-.6,0,0,0,z),{k:9,f:[clock*.25*mv,1,0,1],x:1/z})}
  for(const d of fall)d.s?put(QUAD,trs(d.x,d.y,1.2,0,0,d.p+clock*.6*mv,.08+.035*sin(d.p)),{k:5,a:flake[0],z:flake[1],f:[0,sat(P.sn),0,dark?.4:1],x:12}):put(QUAD,trs(d.x,d.y,1.2,0,0,.1,.018,.17,1),{k:6,a:wet,f:[0,.9*sat(P.rn),0,dark?.3:1]});
  const O=mul(trs(bx,by,0,0,0,0,1),trs(0,0,0,.5,0,-.3,mix(1.34,1.75,P.op))),a=ring%TAU,cx=cos(a),sz=sin(a);
  sp=[O[0]*cx+O[8]*sz+O[12]-bx,O[1]*cx+O[9]*sz+O[13]-by,O[2]*cx+O[10]*sz];
  if(P.ro>.01){const t=a/TAU,ad=dark?.3:1;
    put(HAZE,O,{k:4,a:ink[0],z:ink[1],f:[t,P.ro*.3,0,ad]});put(HOOP,O,{k:4,a:ink[0],z:ink[1],f:[t,P.ro,0,ad]});
    const z=.3*(1+.15*sin(clock*9)*mv);put(QUAD,trs(sp[0]+bx,sp[1]+by,sp[2],0,0,clock*1.5*mv,z),{k:5,a:GOLD[0],z:GOLD[1],f:[0,min(1,P.ro*1.6),0,dark?.4:1],x:1/z})}
  /* the laptop rises in front of it, lid toward us */
  if(gear)put(QUAD,trs(bx,by-.8-(1-P.hk)*1.7,1.18,-.22,0,0,.8,.5,1),{k:8,a:GLOW});
  for(const b of bits){const z=b.s*sin(PI*b.t/b.d);put(QUAD,trs(b.x,b.y,1.3,0,0,b.r+b.t*3,z),{k:5,a:ink[3],z:ink[4],f:[0,1,0,dark?.4:1],x:1/max(z,.01)})}}

/* ---- the lock's own scene: the home itself, a small house. While a passkey is checked the things of the house circle it and its signal rises.
   When the passkey is good they fly home, the house turns once and its windows light, and a ring of light goes out from it */
function house(){
  const t=clock,ck=sat(hc),w=hi<0?0:t-hi,inn=hi<0?0:ease(seg(w,0,.55)),lamp=hi<0?.22+.5*ck*(.6+.4*sin(t*5)):mix(.3,1,seg(w,.45,.6))+.1*sin(t*4),
    q=1+(hi<0?0:.16*sin(seg(w,.5,.9)*PI))+.012*sin(t*2.3),R=trs(0,-.12+.03*sin(t*1.7),0,.16,.55+.3*sin(t*.8)*ck+inn*TAU,0,1/sqrt(q),q,1/sqrt(q)),B=m=>mul(R,m);
  gl.uniform4f(U.uC,0,0,0,1);gl.uniform3f(U.uG,0,0,0);gl.uniform1f(U.uH,0);gl.uniform3f(U.uW,0,0,0);gl.uniform4f(U.uE,1,0,0,1);
  gl.disable(gl.DEPTH_TEST);gl.enable(gl.BLEND);
  put(QUAD,trs(0,0,-2,0,0,0,2.6),{k:6,a:ink[2],f:[0,(.25+.3*ck+.5*lamp)*(dark?.5:.3),0,dark?.15:1]});
  gl.enable(gl.DEPTH_TEST);gl.disable(gl.BLEND);
  put(BOX,B(trs(0,-.2,0,0,0,0,.62,.46,.56)),{k:11,a:WALL,z:tint[1],s:SHADOW,f:[lamp,0,0,1]});
  put(ROOF,B(trs(0,.26,0,0,0,0,.78,.52,.7)),{a:tint[0],z:tint[1],f:[.25,0,0,1]});
  put(BOX,B(trs(.36,.6,-.12,0,0,0,.09,.2,.09)),{a:WALL,z:WALL,s:SHADOW});
  /* the things of the house, circling it: a lamp, a speaker, a television and a camera */
  const O=trs(0,-.1,0,.42,0,-.12,max(.01,1.75*ck*(1-inn)));
  if(ck>.02&&inn<1)for(let i=0;i<4;i++){const a=t*1.25+i*PI/2+inn*5,c=cos(a),s=sin(a),z=max(.01,.9*sat(ck*1.4)*(1-inn)),
      D=m=>mul(trs(O[0]*c+O[8]*s,O[1]*c+O[9]*s+O[13]+.06*sin(t*3+i),O[2]*c+O[10]*s,.1,.5*sin(t*1.1+i),0,z),m);
    if(i<1){put(BALL,D(trs(0,.04,0,0,0,0,.17)),{a:LAMP,z:LAMP,s:WARM,f:[.6,0,.45+.25*sin(t*6),1]});put(BOX,D(trs(0,-.19,0,0,0,0,.07,.06,.07)),{a:METAL,z:METAL})}
    else if(i<2){put(BOX,D(trs(0,0,0,0,0,0,.13,.2,.11)),{a:DARK,z:DARK,f:[.3,0,0,1]});put(BALL,D(trs(0,-.04,.11,0,0,0,.085,.085,.03)),{a:METAL,z:METAL,f:[.5,0,0,1]})}
    else if(i<3){put(BOX,D(trs(0,0,0,0,0,0,.25,.16,.025)),{a:DARK,z:DARK,f:[.3,0,0,1]});put(BOX,D(trs(0,0,.012,0,0,0,.22,.13,.02)),{a:tint[1],z:tint[0],f:[.4,0,.5,1]})}
    else{put(BOX,D(trs(0,0,0,0,0,0,.16,.12,.1)),{a:WALL,z:WALL,s:SHADOW});put(BALL,D(trs(0,0,.1,0,0,0,.075,.075,.04)),{k:2,x:13})}}
  gl.enable(gl.BLEND);gl.depthMask(false);
  /* the path they keep, and the signal rising from the roof */
  if(ck>.02){put(HOOP,O,{k:4,a:ink[0],z:ink[1],f:[0,.35*ck*(1-inn),1,dark?.3:1]});
    for(let i=0;i<3;i++){const p=(t*.55+i/3)%1;put(HAZE,B(trs(0,.75+p*.9,0,0,0,0,.12+p*.8)),{k:4,a:ink[0],z:ink[1],f:[0,(1-p)*ck*.8,1,dark?.3:1]})}}
  if(hi>=0&&w<1.1){const p=ease(seg(w,.5,1.1));put(HAZE,trs(0,-.1,0,PI/2,0,0,.2+p*2.4),{k:4,a:GOLD[0],z:GOLD[1],f:[0,(1-p)*seg(w,.5,.56),1,dark?.3:1]})}
  for(const b of bits){const z=b.s*sin(PI*b.t/b.d);put(QUAD,trs(b.x,b.y,1.3,0,0,b.r+b.t*3,z),{k:5,a:GOLD[0],z:GOLD[1],f:[0,1,0,dark?.4:1],x:1/max(z,.01)})}}

const alive=()=>!!host&&host.isConnected&&SHEET.type==='swifty'&&!SHEET.el.hidden&&!document.hidden;
const locked=()=>scene==='home'&&!$('#lock').hidden&&!document.hidden;
/* one frame at a time, at most sixty a second, and none at all once the chat is closed, the lock is cleared or the tab is out of view. Stopping also lets go
   of the canvas's memory. The house is left standing as one still frame whenever nothing is asked of it */
function tick(ts){raf=0;if(lost)return;const door=scene==='home';
  if(door?!locked():!alive()){if(!door||$('#lock').hidden)cv.width=cv.height=1;return}
  if(prev&&ts-prev<12){raf=requestAnimationFrame(tick);return}
  const dt=prev?min(.034,(ts-prev)/1000):.016;prev=ts;
  if(door){clock+=dt;hc=mix(hc,busy||hi>=0?1:0,1-Math.exp(-dt*5));
    if(hi>=0&&clock-hi>.5&&!popped){popped=true;for(let i=0;i<12;i++)bits.push({x:dice(-.7,.7),y:dice(-.4,.5),vx:dice(-2.6,2.6),vy:dice(.6,3),t:0,d:dice(.5,1),s:dice(.1,.22),r:dice(0,3)})}
    bits=bits.filter(b=>{b.t+=dt;b.x+=b.vx*dt;b.y+=b.vy*dt;b.vy-=3.5*dt;return b.t<b.d});draw();if(busy||hi>=0||hc>.01)raf=requestAnimationFrame(tick);return}
  step(dt);draw();raf=requestAnimationFrame(tick)}
function wake(){if(ok&&!lost&&!reduced&&!raf&&(scene==='home'?locked():alive())){prev=0;raf=requestAnimationFrame(tick)}}
/* one frame, already settled: for reduced motion, and to place the flat drawing */
function still(){if(!alive())return;fl=null;const tg=spot();stand(tg,tg);
  /* the code behind it is poured once and then left standing */
  if(hack&&(!soaked||rain.parentNode!==host.parentNode)){for(let i=0;i<60;i++)pour();soaked=true}
  if((sky==='rain'||sky==='snow')&&!fall.length)for(let i=0;i<16;i++){const d=drip(sky==='snow',1);if(d.s||abs(d.x-UX)>1.25||d.y>UY+.7)fall.push(d)}
  if(!ok||lost)return;aim();for(const k in K){P[k]=T[k];V[k]=0}draw()}

/* the one canvas, made the first time either scene needs it. True when it can draw */
function ensure(){
  if(!fig){
    fig=document.createElement('div');fig.className='fig';fig.innerHTML=ART+'<i></i>';flat=fig.firstChild;cv=document.createElement('canvas');fig.insertBefore(cv,flat);
    $('i',fig).addEventListener('pointerdown',()=>shoot('poke'));
    /* the flat drawing shows whenever the canvas cannot */
    const show=g=>{cv.hidden=!g;flat.toggleAttribute('hidden',g)};
    ok=build();show(ok);
    cv.addEventListener('webglcontextlost',e=>{e.preventDefault();lost=true;show(false);if(lockEl)lockEl.classList.remove('gl')});
    cv.addEventListener('webglcontextrestored',()=>{lost=!build();show(!lost);wake()});
    addEventListener('resize',()=>{if(alive()&&(reduced||!ok))still()});document.addEventListener('visibilitychange',wake)}
  /* on the dark sheet the orbit glows in the drawing's cyan and cream; on the light one it takes the page's own tint. Rain and snow are drawn darker on the light one, to be seen */
  const de=document.documentElement,tok=(n,d)=>{const v=parseInt(cssv(n).slice(1),16);return hex(v>=0?v:d)};
  dark=matchMedia('(prefers-color-scheme: dark)').matches&&de.dataset.theme!=='light'||de.dataset.theme==='dark';
  tint=[tok('--tint-soft',0x7A5BF5),tok('--tint',0x6644DD)];ink=dark?NIGHT:[tint[0],tint[1],tint[0],GOLD[0],GOLD[1]];
  wet=dark?[.62,.8,1]:[.3,.45,.85];flake=dark?[ONE,ICE]:[[.55,.66,.9],[.42,.55,.85]];
  return ok&&!lost}
function mount(root){
  host=$('#swf',root);if(!host)return;
  ensure();if(cv.parentNode!==fig)fig.insertBefore(cv,flat);scene='bird';
  host.appendChild(fig);seen=CHAT.msgs.length;liveMsg=CHAT.msgs.find(m=>m.live)||null;first=true;shot=null;fl=null;bits=[];talk=0;lastT=''}
/* the lock is showing: the canvas moves to its button and draws the house, once. With reduced motion, or without WebGL, the flat mark stays */
function home(){
  lockEl=$('#unlockbtn');if(scene==='home'&&cv.parentNode&&cv.parentNode.parentNode===lockEl)return;
  hi=-1;busy=popped=false;hc=0;if(reduced||!ensure())return;
  let stage=$('.stage',lockEl);if(!stage){stage=document.createElement('span');stage.className='stage';stage.setAttribute('aria-hidden','true');lockEl.appendChild(stage)}
  stage.appendChild(cv);lockEl.classList.add('gl');scene='home';bits=[];
  cv.width=cv.height=Math.round((cv.offsetWidth||stage.offsetWidth)*min(devicePixelRatio||1,2))||600;draw()}
function check(on){busy=on;if(scene==='home')wake()}
/* the passkey was good: the house celebrates, and this waits for the best of it before the page moves on */
function welcome(){busy=false;if(scene!=='home'||!locked()||!ok||lost)return Promise.resolve();hi=clock;wake();return new Promise(r=>setTimeout(r,650))}
/* and then the whole display: rings of light go out from where the house stood, and the home rises into place piece by piece */
function arrive(){if(reduced)return;const b=$('#unlockbtn').getBoundingClientRect(),x=b.left+b.width/2,y=b.top+b.height/2,r=Math.hypot(max(x,innerWidth-x),max(y,innerHeight-y));
  for(let i=0;i<3;i++){const w=document.createElement('i');w.className='wave';w.style.cssText=`left:${x-r}px;top:${y-r}px;width:${2*r}px;height:${2*r}px`;document.body.appendChild(w);
    w.animate([{transform:'scale(.04)',opacity:.85},{transform:'scale(1.12)',opacity:0}],{duration:1100,delay:i*170,easing:'cubic-bezier(.2,.7,.2,1)',fill:'both'}).onfinish=()=>w.remove()}
  $$('#top,#view-fav>*').forEach((el,i)=>el.animate([{opacity:0,transform:'translateY(24px) scale(.97)'},{opacity:1,transform:'none'}],{duration:700,delay:120+i*60,easing:springEasing(),fill:'backwards'}));
  $('#cap-bottom').animate([{opacity:0},{opacity:1}],{duration:500,delay:520,fill:'backwards'})}
function react(m){shoot(m.fail?'sorry':m.mood);talk=clock+(m.fail?.9:clamp((m.text||'').length/26,.9,3.4))+(shot?shot.d*.6:0)}
/* called after every repaint of the chat: reads what is true now, and what just happened */
function sync(){
  if(!host||!host.isConnected)return;
  const n=CHAT.msgs.length,last=CHAT.msgs[n-1],was=hero&&!first;hero=$('#swf-hero',host.parentNode);hack=hacking();
  const air=hack?null:weather();if(air!==sky){sky=air;fall=[]}
  const next=CHAT.hear==='on'?'listen':CHAT.hear||CHAT.busy?(last&&last.live&&last.text?'talk':'think'):'idle';
  if(liveMsg&&!liveMsg.live){react(liveMsg);liveMsg=null}
  for(let i=seen;i<n;i++){const m=CHAT.msgs[i];if(m.who==='me')shoot('send');else if(m.live)liveMsg=m;else react(m)}
  seen=n;if(next!==st){st=next;since=clock}
  if(!alive())return;
  if(reduced||!ok){first=false;still();return}
  const tg=spot(),d=fig.offsetWidth;
  /* into an empty chat it swoops down from the far corner; into a conversation it drops down its own margin, clear of the words; and when the first message is sent it flies from the middle to the margin */
  if(first){first=false;P.op=hero?1:0;
    fl=hero?{a:[tg[0]+host.parentNode.clientWidth*.62,tg[1]-d*tg[2]*.7-40,tg[2]*.42],t:-.12,d:1.15,dip:40*tg[2],hello:1}:{a:[-d*.2,-d*2.3,.55],t:-.12,d:1,dip:14,hello:1};stand(fl.a,tg)}
  else if(was&&!hero&&vis){fl={a:[vis[0]-nest[0],vis[1]+host.parentNode.scrollTop-nest[1],cur[2]],t:0,d:.8,dip:-18,hello:0};stand(fl.a,tg)}
  wake()}
return {mount,sync,home,check,welcome,arrive}})();

export {SWF};
