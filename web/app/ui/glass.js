import {$,$$,clamp,reduced} from '../core/util.js';

/* ============================================================ light through the window: the thing the glass bends
   Slow diagonal bands of light, tinted by the lit bulbs, drift across the room behind every pane of glass.
   The lens rims refract them in Chromium; everywhere else the blur softens them and the sheen stays. */
const RAYS={c:$('#rays'),ctx:null,raf:0,t0:performance.now(),hue:38,bright:0};
function raysFrame(now){
  RAYS.raf=0;const c=RAYS.c;if(!c||document.hidden)return;
  const w=innerWidth,h=innerHeight;const dpr=1;
  if(c.width!==w*dpr||c.height!==h*dpr){c.width=w*dpr;c.height=h*dpr}
  const ctx=RAYS.ctx||(RAYS.ctx=c.getContext('2d'));ctx.setTransform(dpr,0,0,dpr,0,0);ctx.clearRect(0,0,w,h);
  const t=(now-RAYS.t0)/1000;const dark=matchMedia('(prefers-color-scheme: dark)').matches&&document.documentElement.dataset.theme!=='light'||document.documentElement.dataset.theme==='dark';
  const L=50+RAYS.bright*22;const lum=dark?Math.min(72,L):Math.max(50,Math.min(60,L-4));
  ctx.save();ctx.translate(w*0.5,h*0.5);ctx.rotate(-0.42);
  const span=Math.hypot(w,h);
  for(let i=0;i<7;i++){
    const speed=0.012+i*0.0035;const phase=((t*speed+i*0.17)%1);const x=-span*0.9+phase*span*1.8;
    const bw=42+i*23;const a=0.55-i*0.05;
    const g=ctx.createLinearGradient(x,0,x+bw,0);
    const col=`hsl(${(RAYS.hue+i*9)%360} 75% ${lum}%`;
    g.addColorStop(0,col+' / 0)');g.addColorStop(0.18,col+' / '+a.toFixed(2)+')');g.addColorStop(0.82,col+' / '+a.toFixed(2)+')');g.addColorStop(1,col+' / 0)');
    ctx.fillStyle=g;ctx.fillRect(x,-span,bw,span*2);
  }
  ctx.restore();
  /* a faint grid of dust so there is fine detail to bend even between bands */
  ctx.fillStyle=`hsl(${RAYS.hue} 60% ${lum}% / .18)`;
  const step=28,ox=(t*6)%step,oy=(t*3.5)%step;
  for(let y=-step+oy;y<h;y+=step)for(let x=-step+ox;x<w;x+=step){ctx.fillRect(x,y,1.5,1.5)}
  if(!reduced)RAYS.raf=requestAnimationFrame(raysFrame);
}
function raysKick(){if(!RAYS.raf&&RAYS.c)RAYS.raf=requestAnimationFrame(raysFrame)}
document.addEventListener('visibilitychange',()=>{if(!document.hidden)raysKick()});

/* refraction: a lens map per pane of glass, rim only, Chromium only */
const REFRACT=(()=>{try{const ua=navigator.userAgent;const chromium=/Chrom(e|ium)\//.test(ua)&&!/Edge\//.test(ua);return chromium&&CSS.supports('backdrop-filter','url(#x)')&&!reduced&&!matchMedia('(prefers-reduced-transparency: reduce)').matches}catch(e){return false}})();
function lensMap(w,h,r,rim){
  const c=document.createElement('canvas');c.width=w;c.height=h;const ctx=c.getContext('2d');const img=ctx.createImageData(w,h);const d=img.data;
  for(let y=0;y<h;y++)for(let x=0;x<w;x++){
    const px=x+.5-w/2,py=y+.5-h/2;const qx=Math.abs(px)-(w/2-r),qy=Math.abs(py)-(h/2-r);
    const inside=-(Math.min(Math.max(qx,qy),0)+Math.hypot(Math.max(qx,0),Math.max(qy,0))-r);
    let dx=0,dy=0;
    if(inside<rim&&inside>-1){const t=clamp(1-inside/rim,0,1);const mag=t*t;let nx,ny;
      if(qx>0&&qy>0){nx=Math.sign(px)*qx;ny=Math.sign(py)*qy;const l=Math.hypot(nx,ny)||1;nx/=l;ny/=l}else if(qx>qy){nx=Math.sign(px);ny=0}else{nx=0;ny=Math.sign(py)}
      dx=nx*mag;dy=ny*mag}
    const i=(y*w+x)*4;d[i]=Math.round(128+dx*120);d[i+1]=Math.round(128+dy*120);d[i+2]=128;d[i+3]=255;
  }
  ctx.putImageData(img,0,0);return c.toDataURL();
}
const SVGNS='http://www.w3.org/2000/svg';
function lensFilter(key){
  let f=document.getElementById('refract-'+key);if(f)return f;
  f=document.createElementNS(SVGNS,'filter');f.id='refract-'+key;f.setAttribute('x','0');f.setAttribute('y','0');f.setAttribute('width','100%');f.setAttribute('height','100%');f.setAttribute('color-interpolation-filters','sRGB');
  const img=document.createElementNS(SVGNS,'feImage');img.setAttribute('x','0');img.setAttribute('y','0');img.setAttribute('width','100%');img.setAttribute('height','100%');img.setAttribute('preserveAspectRatio','none');img.setAttribute('result','map');img.classList.add('map');
  const dm=document.createElementNS(SVGNS,'feDisplacementMap');dm.setAttribute('in','SourceGraphic');dm.setAttribute('in2','map');dm.setAttribute('scale','48');dm.setAttribute('xChannelSelector','R');dm.setAttribute('yChannelSelector','G');dm.setAttribute('result','d');
  const bl=document.createElementNS(SVGNS,'feGaussianBlur');bl.setAttribute('in','d');bl.setAttribute('stdDeviation','3');bl.setAttribute('result','bl');
  const cm=document.createElementNS(SVGNS,'feColorMatrix');cm.setAttribute('in','bl');cm.setAttribute('type','saturate');cm.setAttribute('values','1.6');
  f.append(img,dm,bl,cm);$('#lenses').appendChild(f);return f;
}
function refractMaps(){
  if(!REFRACT)return;
  for(const el of $$('.g.lens')){const r=el.getBoundingClientRect();if(!r.width||!r.height)continue;const w=Math.round(r.width),h=Math.round(r.height);const key=el.dataset.lens;if(!key)continue;
    const f=lensFilter(key);const map=f.querySelector('.map');if(map.dataset.size===w+'x'+h)continue;map.dataset.size=w+'x'+h;
    const radius=parseFloat(getComputedStyle(el).borderTopLeftRadius)||h/2;
    map.setAttribute('href',lensMap(w,h,Math.min(radius,h/2),Math.min(26,h/2.4)));
    el.style.backdropFilter=`url(#refract-${key})`;el.style.webkitBackdropFilter='';}
}
if(REFRACT)document.documentElement.classList.add('refract');

export {RAYS,raysKick,refractMaps};
