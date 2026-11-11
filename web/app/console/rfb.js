import {$,$$,clamp,hhmm,esc,ICON,PEND} from '../core/util.js';
import {enc} from '../core/api.js';
import {isOwner} from '../core/store.js';
import {run} from '../core/actions.js';
import {toast} from '../ui/toast.js';
import {A} from '../core/catalog.js';
import {pkStep} from '../core/guards.js';
import {vncResponse} from './des.js';

/* ---------- the console inside the page: drawn from state, never a screenshot ---------- */
const CONSOLE={open:false,v:null,ws:null,t0:0,rfb:null};
const ADDM={tab:'vm'};
const FIND={look:'idle',pair:'idle',found:[],lookMsg:'',pairMsg:''};
function copyText(text,btn,el){const done=()=>{btn.textContent='Copied';setTimeout(()=>{if(btn.isConnected)btn.textContent='Copy'},1600)};const fallback=()=>{try{const sel=getSelection();const rg=document.createRange();rg.selectNodeContents(el);sel.removeAllRanges();sel.addRange(rg);btn.textContent='Selected'}catch(e){}};try{if(navigator.clipboard&&navigator.clipboard.writeText)navigator.clipboard.writeText(text).then(done,fallback);else fallback()}catch(e){fallback()}}
/* a small RFB client: enough of the protocol to show a VM's screen and type at it */
function rfbClient(ws,secret,canvas,onEvent){
  let q=new Uint8Array(0),stage='version',W=0,H=0,ctx=null,rects=0,count=0,rect=null,failed=false;
  const send=a=>{if(ws.readyState===1)ws.send(a instanceof Uint8Array?a:new Uint8Array(a))};
  const u16=(b,o)=>(b[o]<<8)|b[o+1],u32=(b,o)=>((b[o]*16777216)+(b[o+1]<<16)+(b[o+2]<<8)+b[o+3]);
  const take=n=>{const r=q.slice(0,n);q=q.slice(n);return r};
  const be16=v=>[(v>>8)&255,v&255],be32=v=>[(v>>>24)&255,(v>>>16)&255,(v>>>8)&255,v&255];
  const fail=t=>{failed=true;onEvent('fail',t);try{ws.close()}catch(e){}};
  const update=inc=>send([3,inc?1:0,...be16(0),...be16(0),...be16(W),...be16(H)]);
  const size=()=>{canvas.width=W;canvas.height=H;ctx=canvas.getContext('2d')};
  function step(){
    switch(stage){
      case 'version':{if(q.length<12)return false;const v=new TextDecoder().decode(take(12));if(!v.startsWith('RFB '))return fail('That is not a VNC console.'),false;send(new TextEncoder().encode('RFB 003.008\n'));stage='types';return true}
      case 'types':{if(q.length<1||q.length<1+q[0])return false;const n=take(1)[0];if(n===0){stage='reason';return true}const t=Array.from(take(n));if(t.includes(1)){send([1]);stage='result'}else if(t.includes(2)){send([2]);stage='challenge'}else return fail('This console asks for a security type this page does not speak.'),false;return true}
      case 'challenge':{if(q.length<16)return false;send(vncResponse(secret,take(16)));stage='result';return true}
      case 'result':{if(q.length<4)return false;const r=u32(take(4),0);if(r===0){send([1]);stage='init';return true}stage='reason';return true}
      case 'reason':{if(q.length<4)return false;const n=u32(q,0);if(q.length<4+n)return false;take(4);return fail(new TextDecoder().decode(take(n))||'The console refused this session.'),false}
      case 'init':{if(q.length<24)return false;const n=u32(q,20);if(q.length<24+n)return false;const b=take(24+n);W=u16(b,0);H=u16(b,2);size();
        send([0,0,0,0,32,24,0,1,...be16(255),...be16(255),...be16(255),16,8,0,0,0,0]);send([2,0,...be16(3),...be32(0),...be32(1),...be32(-223>>>0)]);update(false);stage='msg';onEvent('ready',{w:W,h:H});return true}
      case 'msg':{if(q.length<1)return false;const t=q[0];
        if(t===0){if(q.length<4)return false;rects=u16(q,2);take(4);count=0;stage='rect';return true}
        if(t===1){if(q.length<6)return false;const n=u16(q,4);if(q.length<6+6*n)return false;take(6+6*n);return true}
        if(t===2){take(1);return true}
        if(t===3){if(q.length<8)return false;const n=u32(q,4);if(q.length<8+n)return false;take(8+n);return true}
        return fail('This console sent something this page does not understand.'),false}
      case 'rect':{if(count>=rects){update(true);stage='msg';return true}if(q.length<12)return false;const b=take(12);const enc=(u32(b,8)|0);rect={x:u16(b,0),y:u16(b,2),w:u16(b,4),h:u16(b,6),enc};count++;
        if(enc===-223){W=rect.w;H=rect.h;size();update(false);return true}
        if(enc===0){stage='raw';return true}if(enc===1){stage='copy';return true}
        return fail('This console used an encoding this page does not speak.'),false}
      case 'raw':{const n=rect.w*rect.h*4;if(q.length<n)return false;const px=take(n);if(rect.w&&rect.h){const img=ctx.createImageData(rect.w,rect.h);for(let i=0;i<n;i+=4){img.data[i]=px[i+2];img.data[i+1]=px[i+1];img.data[i+2]=px[i];img.data[i+3]=255}ctx.putImageData(img,rect.x,rect.y)}stage='rect';return true}
      case 'copy':{if(q.length<4)return false;const b=take(4);ctx.drawImage(canvas,u16(b,0),u16(b,2),rect.w,rect.h,rect.x,rect.y,rect.w,rect.h);stage='rect';return true}
    }
    return false;
  }
  const KEYS={Enter:0xff0d,Backspace:0xff08,Tab:0xff09,Escape:0xff1b,Delete:0xffff,Home:0xff50,End:0xff57,PageUp:0xff55,PageDown:0xff56,ArrowLeft:0xff51,ArrowUp:0xff52,ArrowRight:0xff53,ArrowDown:0xff54,Shift:0xffe1,Control:0xffe3,Alt:0xffe9,Meta:0xffe7,Insert:0xff63};for(let i=1;i<=12;i++)KEYS['F'+i]=0xffbd+i;
  const symOf=k=>KEYS[k]||(Array.from(k).length===1?(k.codePointAt(0)<256?k.codePointAt(0):0x01000000|k.codePointAt(0)):0);
  const keyEvent=(sym,down)=>send([4,down?1:0,0,0,...be32(sym)]);
  return {feed(buf){const n=new Uint8Array(q.length+buf.byteLength);n.set(q,0);n.set(new Uint8Array(buf),q.length);q=n;while(!failed&&step());},
    key(k,down){const s=symOf(k);if(s)keyEvent(s,down);return !!s},
    pointer(x,y,mask){if(W)send([5,mask,...be16(clamp(Math.round(x),0,W-1)),...be16(clamp(Math.round(y),0,H-1))])},
    type(text){for(const ch of text){const s=symOf(ch);if(s){keyEvent(s,true);keyEvent(s,false)}}},
    enter(){keyEvent(0xff0d,true);keyEvent(0xff0d,false)}};
}
function openConsole(v,root){
  if(!isOwner()){toast('Only the owner can open a console.','');return}
  const area=$('#con-area',root),acts=$('#con-acts',root);if(!area)return;
  area.hidden=false;acts.hidden=true;
  const restore=ms=>setTimeout(()=>{const a=$('#con-area');if(a&&!CONSOLE.open){a.hidden=true;a.innerHTML='';const b=$('#con-acts');if(b)b.hidden=false}},ms);
  const a=A({action:'console.open',target:v.id,params:{},level:'critical',cls:'vm',owner:true,key:'console-'+v.id,label:'Open a console on the '+v.name,targetName:v.name,deviceName:v.name});a.channel='touch';
  a.onFail=text=>{area.innerHTML=`<div class="failband">${ICON('warn')}<span>${esc(text)}</span></div>`;restore(5000)};
  a.onDone=(rt,run)=>{if(!run.console){area.innerHTML=`<div class="okline">${ICON('check')}<span>Dry-run: no console was opened, and nothing was contacted.</span></div>`;restore(3500);return}startConsole(v,area,run.console)};
  pkStep(area,'Touch to use your passkey for this console session',a,()=>{area.innerHTML=`<div class="acts"><button class="btn primary" disabled>${PEND}<span>Opening the console</span></button></div>`;run(a)});
}
function startConsole(v,area,info){
  area.innerHTML=`<div class="term" id="term" aria-label="Console on the ${esc(v.name)}"><canvas id="con-canvas" tabindex="0" aria-label="The screen of the ${esc(v.name)}. Type here." style="display:block;width:100%;height:auto;outline:none"></canvas></div>
    <div class="cmdline"><input class="field" id="con-in" placeholder="Type a line and press Enter" aria-label="Type a line" autocomplete="off" autocapitalize="off" spellcheck="false"></div>
    <div class="phr">${['uptime','df -h','free -h','top','journalctl -p err -n 5'].map(c=>`<button class="pill" data-cmd="${esc(c)}">${esc(c)}</button>`).join('')}<button class="pill" data-closecon>Close</button></div>
    <p class="p2" id="con-note">Connecting…</p>`;
  const canvas=$('#con-canvas',area),note=$('#con-note',area);
  const ws=new WebSocket((location.protocol==='https:'?'wss:':'ws:')+'//'+location.host+info.path,info.subprotocols);ws.binaryType='arraybuffer';
  CONSOLE.open=true;CONSOLE.v=v;CONSOLE.ws=ws;CONSOLE.t0=Date.now();
  const rfb=rfbClient(ws,info.vnc_credential||'',canvas,(ev,x)=>{if(ev==='ready'){note.textContent=`Session started ${hhmm(new Date())}. It is in History.`;try{canvas.focus()}catch(e){}}else if(ev==='fail')note.textContent=x});
  CONSOLE.rfb=rfb;
  ws.onmessage=e=>rfb.feed(e.data);
  ws.onclose=()=>{if(CONSOLE.ws===ws)endConsole()};
  ws.onerror=()=>{note.textContent='The console could not be reached.'};
  const inp=$('#con-in',area);inp.addEventListener('keydown',e=>{if(e.key==='Enter'){e.preventDefault();rfb.type(inp.value);rfb.enter();inp.value=''}});
  $$('[data-cmd]',area).forEach(b=>b.onclick=()=>{rfb.type(b.dataset.cmd);rfb.enter()});
  $('[data-closecon]',area).onclick=endConsole;
  canvas.addEventListener('keydown',e=>{if(rfb.key(e.key,true))e.preventDefault()});canvas.addEventListener('keyup',e=>{if(rfb.key(e.key,false))e.preventDefault()});
  let mask=0;const at=e=>{const r=canvas.getBoundingClientRect();return [(e.clientX-r.left)/r.width*canvas.width,(e.clientY-r.top)/r.height*canvas.height]};
  canvas.addEventListener('pointermove',e=>{const [x,y]=at(e);rfb.pointer(x,y,mask)});
  canvas.addEventListener('pointerdown',e=>{mask|=1<<e.button;const [x,y]=at(e);rfb.pointer(x,y,mask);canvas.focus()});
  canvas.addEventListener('pointerup',e=>{mask&=~(1<<e.button);const [x,y]=at(e);rfb.pointer(x,y,mask)});
  canvas.addEventListener('contextmenu',e=>e.preventDefault());
}
function endConsole(){if(!CONSOLE.open)return;CONSOLE.open=false;const ws=CONSOLE.ws;CONSOLE.ws=null;CONSOLE.rfb=null;try{ws&&ws.close()}catch(e){}const secs=Math.round((Date.now()-CONSOLE.t0)/1000);
  const area=$('#con-area');if(area){area.innerHTML=`<div class="okline">${ICON('check')}<span>Console closed after ${secs} s. It is in History.</span></div>`;setTimeout(()=>{const a=$('#con-area');if(a&&!CONSOLE.open){a.hidden=true;a.innerHTML='';const acts=$('#con-acts');if(acts)acts.hidden=false}},2400)}else{const acts=$('#con-acts');if(acts)acts.hidden=false}}

export {CONSOLE,ADDM,FIND,copyText,openConsole,endConsole};
