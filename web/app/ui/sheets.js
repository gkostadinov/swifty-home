import {$,hhmm,esc,reduced,ICON,MONTHS} from '../core/util.js';
import {ROOMS,S,person,isOwner,NR,devName} from '../core/store.js';
import {queueHighlights} from './roomlight.js';
import {dupWidths,render} from '../pages/home.js';
import {NORESP,retry,dismissFail} from '../core/actions.js';
import {CONSOLE,endConsole} from '../console/rfb.js';
import {VOICE,cancelListening} from '../swifty/voice.js';
import {lock,signOut} from './lock.js';
import {enterWall} from './dock.js';
import {setView} from './views.js';
import {SHEETS} from '../main.js';

/* ============================================================ sheets: one surface that rises from the bottom */
const SHEET={type:null,opener:null,stack:[],update:null,close:null,el:$('#sheet'),open:false};
/* a page that holds something on the hub (a setup in progress) lets it go when it is left, whichever way */
function leavePage(){const c=SHEET.close;SHEET.close=null;if(c){try{c()}catch(e){}}}
function springEasing(){const v=getComputedStyle(document.documentElement).getPropertyValue('--spring').trim();return v||'cubic-bezier(.22,1.18,.36,1)'}
function openSheet(type,opener,opts){
  opts=opts||{};const el=SHEET.el;const wasOpen=SHEET.open;if(CONSOLE.open)endConsole();
  const sb=$('#sbody');const prevType=SHEET.type;const prevScroll=sb.scrollTop;
  if(wasOpen&&!opts.replace)SHEET.stack.push({type:SHEET.type,scroll:prevScroll});
  if(!wasOpen){SHEET.stack=[];SHEET.opener=opener||document.activeElement}
  SHEET.type=type;SHEET.open=true;
  if(prevType!==type||!wasOpen)leavePage();
  const [kind,...rest]=type.split(':');const arg=rest.join(':');const r=SHEETS[kind](arg,opts);
  $('#sheet-title').textContent=r.title;sb.innerHTML=r.html;SHEET.update=r.update||null;SHEET.close=r.close||null;
  $('#sheet-back').hidden=!SHEET.stack.length;
  el.style.setProperty('--gh',r.glowHue!=null?r.glowHue:270);el.style.setProperty('--ga',r.glowA!=null?r.glowA:0);
  if(!wasOpen){el.hidden=false;$('#dim').hidden=false;requestAnimationFrame(()=>$('#dim').classList.add('show'));$('#app').inert=true;$('#cap-bottom').inert=true;if(S.wall)$('#wall').inert=true;document.body.classList.add('sheet-open');morphIn(opener)}
  else if(!reduced){$('#sbody').animate([{opacity:0,transform:'translateY(10px)'},{opacity:1,transform:'none'}],{duration:400,easing:springEasing()})}
  /* a new page starts at the top; a refresh of the same page, or Back, keeps its place. Set once the sheet is visible, or it does not take. */
  const target=opts.scroll!=null?opts.scroll:(wasOpen&&opts.replace&&prevType===type)?prevScroll:0;sb.scrollTop=target;sb.scrollLeft=0;
  r.mount&&r.mount($('#sbody'));
  dupWidths();queueHighlights();
  if(!opts.focus)setTimeout(()=>$('#sheet-title').focus({preventScroll:true}),wasOpen?0:80);
}
function morphIn(opener){
  const el=SHEET.el;if(reduced)return;
  const to=el.getBoundingClientRect();let from=null;
  if(opener&&opener.getBoundingClientRect){const r=opener.getBoundingClientRect();if(r.width&&r.height)from=r}
  if(!from){el.animate([{transform:'translate(-50%,110%)'},{transform:'translate(-50%,0)'}],{duration:650,easing:springEasing()});return}
  const sx=Math.max(.05,from.width/to.width),sy=Math.max(.05,from.height/to.height);
  const dx=(from.left+from.width/2)-(to.left+to.width/2),dy=(from.top+from.height/2)-(to.top+to.height/2);
  el.animate([{transform:`translate(calc(-50% + ${dx}px),${dy}px) scale(${sx},${sy})`,opacity:.35},{transform:'translate(-50%,0) scale(1,1)',opacity:1}],{duration:700,easing:springEasing()});
  $('#sbody').animate([{opacity:0},{opacity:0,offset:.3},{opacity:1}],{duration:700});
  $('#shead').animate([{opacity:0},{opacity:0,offset:.3},{opacity:1}],{duration:700});
}
function closeSheet(){
  if(!SHEET.open)return;if(VOICE.on&&SHEET.type===(VOICE.into==='chat'?'swifty':'voice'))cancelListening();const el=SHEET.el;if(CONSOLE.open)endConsole();SHEET.open=false;SHEET.update=null;leavePage();
  $('#dim').classList.remove('show');
  document.body.classList.remove('sheet-open');
  const fin=()=>{if(SHEET.open)return;el.hidden=true;$('#dim').hidden=true;el.style.transform='';$('#app').inert=S.locked;$('#cap-bottom').inert=S.locked;$('#wall').inert=S.locked;const op=SHEET.opener;SHEET.opener=null;SHEET.type=null;if(op&&op.isConnected&&!S.locked){try{op.focus({preventScroll:true})}catch(e){}}render()};
  if(reduced){fin();return}
  const anim=el.animate([{transform:getComputedStyle(el).transform==='none'?'translate(-50%,0)':getComputedStyle(el).transform,opacity:1},{transform:'translate(-50%,110%)',opacity:.6}],{duration:380,easing:'cubic-bezier(.4,0,.6,1)'});
  let done=false;const once=()=>{if(done)return;done=true;fin()};anim.onfinish=once;setTimeout(once,480);
}
function sheetBack(){const prev=SHEET.stack.pop();if(!prev){closeSheet();return}openSheet(prev.type,null,{replace:true,scroll:prev.scroll})}
$('#sheet-close').onclick=closeSheet;$('#sheet-back').onclick=sheetBack;$('#dim').onclick=closeSheet;
/* swipe down to dismiss */
(function(){const el=SHEET.el;let st=null;
  el.addEventListener('pointerdown',e=>{if(e.target.closest('.sbody')&&$('#sbody').scrollTop>0)return;if(e.target.closest('.slider,.hold,button,input,details'))return;st={y:e.clientY,id:e.pointerId,t:Date.now(),dy:0};});
  el.addEventListener('pointermove',e=>{if(!st||e.pointerId!==st.id)return;st.dy=Math.max(0,e.clientY-st.y);if(st.dy>4){try{el.setPointerCapture(e.pointerId)}catch(err){}el.style.transition='none';el.style.transform=`translate(-50%,${st.dy}px)`}});
  const end=e=>{if(!st||e.pointerId!==st.id)return;const s=st;st=null;const fast=s.dy>40&&Date.now()-s.t<260;el.style.transition='';if(s.dy>140||fast){closeSheet()}else{el.style.transition='transform .6s '+springEasing();el.style.transform='';setTimeout(()=>{el.style.transition=''},600)}};
  el.addEventListener('pointerup',end);el.addEventListener('pointercancel',end);
})();

/* ---------- pieces ---------- */
const row=(icon,b,small,v,attrs)=>`<div class="rowi ${attrs?'btnrow':''}" ${attrs||''} ${attrs?'role="button" tabindex="0"':''}>${icon?ICON(icon):''}<div class="t"><b>${b}</b>${small?`<small>${small}</small>`:''}</div>${v!=null?`<span class="v num">${v}</span>`:''}${attrs?'<svg class="ic chev" aria-hidden="true"><use href="#i-chev"/></svg>':''}</div>`;
const roomOf=key=>ROOMS[key]||null;
function failHtml(key){const n=NORESP.find(x=>x.key===key);if(!n)return '';return `<div class="failband">${ICON('warn')}<span>${esc(n.text)}</span><span class="acts"><button class="btn" data-retry="${n.id}">Retry</button><button class="btn" data-dismiss="${n.id}">Dismiss</button></span></div>`}
const sessionRow=()=>`<div class="sessrow">${ICON(isOwner()?'key':'person')}<div class="t"><b>${esc(person().name)}, with ${esc(devName())}</b><small>Since ${S.session?hhmm(new Date(S.session.created_at)):''} · ${isOwner()?'owner':'family'} · passkeys only</small></div><button class="pill" data-act="lock" aria-label="Lock this screen">${ICON('lock')}Lock</button></div>`;
const fmtDay=ms=>{const d=new Date(ms);return d.getDate()+' '+MONTHS[d.getMonth()].slice(0,3)+' '+d.getFullYear()};
const fmtWhen=ms=>{const d=new Date(ms);const t=new Date();return (d.toDateString()===t.toDateString()?'today ':d.getDate()+' '+MONTHS[d.getMonth()].slice(0,3)+' ')+hhmm(d)};
const notReportingBand=why=>`<div class="failband">${ICON('warn')}<span>${esc(NR)}. ${esc(why||'')}</span></div>`;
/* the sheet body is wired once; whatever a sheet renders, these handlers survive */
(function wireSheetBody(){const root=$('#sbody');
  root.addEventListener('click',e=>{
    if(e.target.closest('[data-guard]'))return;
    const r=e.target.closest('[data-retry]');if(r){retry(r.dataset.retry);return}
    const d=e.target.closest('[data-dismiss]');if(d){dismissFail(d.dataset.dismiss);return}
    const rp=e.target.closest('[data-room-pick]');if(rp){openSheet('where:'+rp.dataset.roomPick,rp);return}
    const sh=e.target.closest('[data-open]');if(sh){openSheet(sh.dataset.open,sh);return}
    const a=e.target.closest('[data-act]');if(a){const k=a.dataset.act;
      if(k==='wall'){closeSheet();enterWall()}else if(k==='rooms'){closeSheet();setView('rooms')}else if(k==='home'){closeSheet();setView('fav')}else if(k==='lock'){closeSheet();lock()}else if(k==='signout'){closeSheet();signOut()}}
  });
  root.addEventListener('keydown',e=>{if((e.key==='Enter'||e.key===' ')&&e.target.matches('.rowi[role="button"]')){e.preventDefault();e.target.click()}});
})();
function refreshSheet(){if(SHEET.open&&!$('[data-guard]',SHEET.el)&&!CONSOLE.open)openSheet(SHEET.type,null,{replace:true,focus:'keep'})}

export {SHEET,springEasing,openSheet,closeSheet,sheetBack,row,roomOf,failHtml,sessionRow,fmtDay,fmtWhen,notReportingBand,refreshSheet};
