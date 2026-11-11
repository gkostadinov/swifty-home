import {$,$$,clamp,esc} from '../core/util.js';
import {D} from '../core/store.js';
import {roomBulbs,lightsSummary,paintTile,render} from '../pages/home.js';
import {ACT} from '../core/catalog.js';
import {dispatch} from '../core/guards.js';
import {openSheet} from './sheets.js';
import {touched} from './dock.js';

/* ============================================================ gestures: horizontal drag sets a level, tap opens the sheet */
function attachSlider(el,o){
  let st=null;
  el.addEventListener('pointerdown',e=>{if(e.pointerType==='mouse'&&e.button!==0)return;st={x:e.clientX,y:e.clientY,t:Date.now(),id:e.pointerId,axis:null,rect:el.getBoundingClientRect(),p:null};el.classList.add('held');touched()});
  el.addEventListener('pointermove',e=>{if(!st||e.pointerId!==st.id)return;const dx=e.clientX-st.x,dy=e.clientY-st.y;
    if(!st.axis){if(Math.abs(dx)>8&&Math.abs(dx)>Math.abs(dy)*1.3){st.axis='x';try{el.setPointerCapture(e.pointerId)}catch(err){}el.classList.add('dragging');el.classList.remove('held')}else if(Math.abs(dy)>8){st.axis='y';el.classList.remove('held');return}else return}
    if(st.axis!=='x')return;st.p=clamp((e.clientX-st.rect.left)/st.rect.width*100,0,100);o.drag(st.p)});
  const end=e=>{if(!st||e.pointerId!==st.id)return;const s=st;st=null;el.classList.remove('held');
    if(s.axis==='x'){el.classList.remove('dragging');if(s.p!=null)o.set(Math.round(s.p))}
    else if(!s.axis&&e.type==='pointerup'&&Date.now()-s.t<800)o.tap(e);
    else render()};
  el.addEventListener('pointerup',end);
  el.addEventListener('pointercancel',e=>{if(st&&e.pointerId===st.id){st=null;el.classList.remove('held','dragging');render()}});
  el.addEventListener('keydown',e=>{if(e.key==='Enter'||e.key===' '){e.preventDefault();o.tap(e)}else if(e.key==='ArrowRight'||e.key==='ArrowUp'){e.preventDefault();o.set(clamp(o.get()+5,0,100))}else if(e.key==='ArrowLeft'||e.key==='ArrowDown'){e.preventDefault();o.set(clamp(o.get()-5,0,100))}else if(e.key==='Home'){e.preventDefault();o.set(0)}else if(e.key==='End'){e.preventDefault();o.set(100)}});
}
function tileDrag(el,kind,room){return p=>{el.style.setProperty('--p',p.toFixed(1));const txt=kind==='lights'?Math.round(p)+'%':String(Math.round(p));$$('.big',el).forEach(b=>b.textContent=txt);if(kind==='lights'){const L=lightsSummary(room?roomBulbs(room):null);$$('.state',el).forEach(s=>s.textContent=p===0?'All off':L.n?L.state:'All on');$('.fill',el).classList.toggle('none',p===0)}}}
/* room is a name, or a function when the tile follows the room picked on Home */
function wireTile(el,kind,room){
  if(!el)return;const scope=()=>typeof room==='function'?room():room;paintTile(el,kind,scope());
  attachSlider(el,{get:()=>{const r=scope();return kind==='lights'?lightsSummary(r?roomBulbs(r):null).avg:D.speaker.vol},drag:p=>{const r=scope();if(!(r&&!roomBulbs(r).length))tileDrag(el,kind,r)(p)},
    set:v=>{const r=scope();if(r&&!roomBulbs(r).length){render();return}dispatch(kind==='lights'?(r?ACT.roomLevel(r,v):ACT.lightsLevel(v)):ACT.vol(v),{channel:'touch'})},
    tap:()=>{const r=scope();openSheet(r?'room:'+r:kind,el)}});
}
function sliderEl(id,p,lit,hue,label,valText){return `<div class="slider ${lit?'lit':''}" id="${id}" role="slider" tabindex="0" aria-label="${esc(label)}" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${Math.round(p)}" style="--p:${p};--gh:${hue||38}"><div class="slab base"><span>${esc(label)}</span><span class="num">${esc(valText)}</span></div><div class="sfill ${lit?'lit':''}"><div class="slab"><span>${esc(label)}</span><span class="num">${esc(valText)}</span></div></div></div>`}
function wireSlider(el,o){
  attachSlider(el,{get:o.get,drag:p=>{el.style.setProperty('--p',p.toFixed(1));$$('.num',el).forEach(n=>n.textContent=o.fmt(Math.round(p)))},set:v=>o.set(v),tap:e=>{const r=el.getBoundingClientRect();const p=clamp((e.clientX-r.left)/r.width*100,0,100);o.set(Math.round(p))}});
}

export {wireTile,sliderEl,wireSlider};
