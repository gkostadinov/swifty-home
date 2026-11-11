import {$,$$} from '../core/util.js';
import {slug,D} from '../core/store.js';
import {problems} from '../core/status.js';
import {RAYS,refractMaps} from './glass.js';
import {dupWidths} from '../pages/home.js';
import {PENDING} from '../core/actions.js';
import {roomOf} from './sheets.js';

/* ============================================================ the room as a light source */
function lean(key,field,cur){const a=PENDING[key];return a&&a.lean&&a.lean[field]!=null?a.lean[field]:cur}
function bulbLight(b){const on=lean('bulb'+b.id,'on',b.on?1:0);const bri=lean('bulb'+b.id,'bri',b.bri);const all=PENDING.lights||PENDING['room-'+slug(roomOf('bulb'+b.id)||'none')];let o=on,br=bri;if(all&&all.lean){if(all.lean.on!=null)o=all.lean.on;if(all.lean.bri!=null)br=all.lean.bri}return {a:o*(0.25+0.75*br/100),hue:b.hue,bri:br,on:o}}
function paintScene(){
  const blooms=$$('#scene .bloom');
  let hx=0,hy=0,tw=0,bright=0;
  D.bulbs.slice(0,4).forEach((b,i)=>{const L=bulbLight(b);blooms[i].style.setProperty('--h',b.hue);blooms[i].style.setProperty('--a',L.a.toFixed(3));hx+=Math.cos(b.hue/180*Math.PI)*L.a;hy+=Math.sin(b.hue/180*Math.PI)*L.a;tw+=L.a;bright=Math.max(bright,L.a)});
  RAYS.hue=tw>0.01?((Math.atan2(hy,hx)*180/Math.PI)+360)%360:262;RAYS.bright=bright;
  $('#scene').classList.toggle('cool',problems().length>0);
}
/* each tile's edge highlight angles toward the nearest lit bloom */
let hlRaf=0;
function highlights(){
  hlRaf=0;const W=innerWidth,H=innerHeight;
  const lights=D.bulbs.slice(0,4).map((b,i)=>{const L=bulbLight(b);return {x:[0,W,0,W][i],y:[0,0,H,H][i],w:L.a,h:b.hue}}).filter(l=>l.w>0.02);
  for(const el of $$('.g')){
    if(el.hidden||el.closest('[hidden]'))continue;
    const r=el.getBoundingClientRect();if(!r.width)continue;
    const cx=r.left+r.width/2,cy=r.top+r.height/2;let vx=0,vy=0,hue=0,tw=0;
    for(const l of lights){const dx=l.x-cx,dy=l.y-cy;const d=Math.hypot(dx,dy)||1;const w=l.w/d;vx+=dx/d*w;vy+=dy/d*w;hue+=l.h*w;tw+=w}
    if(tw<1e-6){vx=0;vy=-1;hue=270}else hue/=tw;
    const ang=(Math.atan2(vx,-vy)*180/Math.PI+180+360)%360;
    el.style.setProperty('--hl',ang.toFixed(0)+'deg');
    el.style.setProperty('--hlc',tw<1e-6?'':`hsl(${hue.toFixed(0)} 90% ${tw<1e-6?'96%':'86%'})`);
  }
}
function queueHighlights(){if(!hlRaf)hlRaf=requestAnimationFrame(highlights)}
addEventListener('scroll',queueHighlights,{passive:true});addEventListener('resize',()=>{queueHighlights();refractMaps();dupWidths()});

export {lean,bulbLight,paintScene,queueHighlights};
