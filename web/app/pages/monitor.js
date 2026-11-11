import {$,$$,clamp,pad,hhmm,esc,store,ICON,DAYS} from '../core/util.js';
import {api,enc} from '../core/api.js';
import {HEALTHDATA,TGT,NR,T,VMS,FAULTS,faultSection,faultValue} from '../core/store.js';
import {cpuText,ownersByThread,runsOf} from '../core/hostdata.js';
import {machineName} from '../core/status.js';
import {SHEET,row,notReportingBand} from '../ui/sheets.js';

/* ---------- histories for the charts: fetched, kept for a minute, and empty when there is nothing ---------- */
const RANGES={'1h':{label:'1 hour'},'24h':{label:'24 hours'},'7d':{label:'7 days'}};
let RANGE=(()=>{const r=store.get('range','1h');return RANGES[r]?r:'1h'})();
const HIST_TTL=60e3,HDATA={};
/* the host's own load from the host, power and temperature from the device that collects them, a computer's load from its host */
function histSource(key){if(key==='load')return TGT.host?{path:TGT.host,field:'cpu',scale:100}:null;if(key==='pkgW')return TGT.telemetry?{path:TGT.telemetry,metric:'pkg_watts'}:null;if(key==='pkgC')return TGT.telemetry?{path:TGT.telemetry,metric:'pkg_temp_c'}:null;const m=key.match(/^vm:(.+)$/);return m?{path:m[1],field:'cpu',scale:100}:null}
async function histFetch(key,range,h){const src=histSource(key);if(!src){h.at=Date.now();h.arr=[];h.times=[];return}if(h.busy)return;h.busy=true;
  try{const r=await api('GET','/history/'+enc(src.path)+'?range='+range+(src.metric?'&metric='+enc(src.metric):''));
    const rows=src.metric?((r.series||{})[src.metric]||[]).map(p=>[p[0],p[1]]):(r.points||[]).filter(p=>typeof p[src.field]==='number'&&typeof p.time==='number').map(p=>[p.time*1000,p[src.field]*(src.scale||1)]);
    h.times=rows.map(x=>x[0]);h.arr=rows.map(x=>x[1])}catch(e){h.arr=[];h.times=[]}
  h.busy=false;h.at=Date.now();if(SHEET.open)paintCharts($('#sbody'))}
function chartGet(key,o){const k=key+'|'+RANGE;const h=HDATA[k]||(HDATA[k]={at:0,busy:false,arr:[],times:[]});if(Date.now()-h.at>HIST_TTL)histFetch(key,RANGE,h);return {arr:h.arr,o:Object.assign({times:h.times,range:RANGE,empty:h.at?(h.arr.length===1?'Only one reading so far':NR):'Loading…'},o)}}
const lastOf=key=>{const h=HDATA[key+'|'+RANGE];return h&&h.arr.length?h.arr[h.arr.length-1]:null};
/* canvases, drawn to scale */
function fitCanvas(c,h){const dpr=Math.min(2,devicePixelRatio||1);const w=c.clientWidth||300;const hh=h||c.clientHeight||80;if(c.width!==Math.round(w*dpr)||c.height!==Math.round(hh*dpr)){c.width=Math.round(w*dpr);c.height=Math.round(hh*dpr)}const ctx=c.getContext('2d');ctx.setTransform(dpr,0,0,dpr,0,0);return {ctx,w,h:hh}}
const cssv=n=>getComputedStyle(document.documentElement).getPropertyValue(n).trim();
function drawStrip(c,arr,min,max,limit){if(!c)return;const {ctx,w,h}=fitCanvas(c,80);ctx.clearRect(0,0,w,h);const y=v=>h-6-clamp((v-min)/(max-min),0,1)*(h-12);
  ctx.strokeStyle=cssv('--line');ctx.lineWidth=1;ctx.beginPath();ctx.moveTo(0,y(min)+.5);ctx.lineTo(w,y(min)+.5);ctx.stroke();
  if(limit!=null){ctx.strokeStyle=cssv('--red');ctx.setLineDash([4,4]);ctx.beginPath();ctx.moveTo(0,y(limit)+.5);ctx.lineTo(w,y(limit)+.5);ctx.stroke();ctx.setLineDash([])}
  const n=arr.length;if(n>1){ctx.strokeStyle=cssv('--tint-soft');ctx.lineWidth=1.6;ctx.beginPath();arr.forEach((v,i)=>{const x=i/(n-1)*w;i?ctx.lineTo(x,y(v)):ctx.moveTo(x,y(v))});ctx.stroke()}}
/* the load on each thread, as the collector reports it, grouped by the computer pinned to it when the collector says so. Nothing
   here knows how many threads there are or what runs where: the owners come from each computer's pinned threads, the kinds from the
   collector's list, and the names from the computers this hub knows by the same VM number */
const KIND_WORD={performance:'Performance',efficiency:'Efficiency'},KIND_SHORT={performance:'P',efficiency:'E'};
const guestName=g=>{const v=VMS.find(x=>String(x.vmid)===String(g.id)&&(!x.host||x.host==='primary'));return (v&&v.short)||g.name||('VM '+g.id)};
const guestColour=g=>cssv('--core-'+g.slot);
const coreOwners=()=>T.cores?ownersByThread(T.cores.length,T.guests):[];
/* the computers that are running and may use any thread; one whose pinning is unknown is not named */
const anyThread=()=>T.guests.filter(g=>g.affinity===null&&g.running);
function coreSummary(){const n=T.cores?T.cores.length:0;if(!n)return 'Load on each thread. '+NR+'.';
  const pinned=T.guests.filter(g=>g.affinity),any=anyThread();
  let s='Load on each of the '+n+' threads, as the collector reports it';
  if(pinned.length)s+=', coloured by the computer pinned to it. '+pinned.map(g=>guestName(g)+': threads '+cpuText(g.affinity)+'.').join(' ');else s+='.';
  if(any.length)s+=' Runs on any thread: '+any.map(guestName).join(', ')+'.';
  return s}
/* what stands under the chart: which colour is which computer, and the computers that are not pinned */
function coreLegendInner(){const pinned=T.guests.filter(g=>g.affinity),any=anyThread();
  return (pinned.length?`<div class="legend">${pinned.map(g=>`<span><i style="background:var(--core-${g.slot})"></i>${esc(guestName(g))} · threads ${esc(cpuText(g.affinity))}</span>`).join('')}</div>`:'')+(any.length?`<p class="fine">Runs on any thread: ${any.map(g=>esc(guestName(g))).join(', ')}.</p>`:'')}
function drawCores(c){if(!c)return;const {ctx,w,h}=fitCanvas(c,132);ctx.clearRect(0,0,w,h);ctx.font='13px '+cssv('--font');
  const u=T.cores;if(!u||!u.length){ctx.fillStyle=cssv('--text-2');ctx.textAlign='center';ctx.textBaseline='middle';ctx.fillText(NR,w/2,h/2);ctx.textAlign='left';return}
  const n=u.length,owners=coreOwners(),pinned=owners.some(o=>o.length),kinds=T.coreKinds;
  const gap=n>48?1:2,top=pinned?26:8,bot=kinds?20:8;const lw=Math.max(1,(w-(n-1)*gap)/n),H=h-top-bot,r=Math.min(4,lw/2);
  const text2=cssv('--text-2'),base=cssv('--core-1');
  /* a name cut to fit with an ellipsis, or nothing when not even a few letters fit; the legend under the chart names every colour */
  const clip=(t,room)=>{let k=t.length;while(k>3&&ctx.measureText(t.slice(0,k).trimEnd()+'…').width>room)k--;return k>=3&&ctx.measureText(t.slice(0,k).trimEnd()+'…').width<=room?t.slice(0,k).trimEnd()+'…':''};
  /* a shape filled in one colour, or in side-by-side slices of several when threads are shared */
  const fill=(x,y,bw,bh,rad,cols,alpha)=>{ctx.save();ctx.beginPath();if(ctx.roundRect)ctx.roundRect(x,y,bw,bh,rad);else ctx.rect(x,y,bw,bh);ctx.clip();ctx.globalAlpha=alpha;cols.forEach((col,k)=>{ctx.fillStyle=col;ctx.fillRect(x+k*bw/cols.length,y,bw/cols.length+.5,bh)});ctx.restore()};
  for(let i=0;i<n;i++){const x=i*(lw+gap);const own=owners[i];const cols=own.length?own.map(guestColour):[pinned?text2:base];const bh=Math.max(3,u[i]*H);
    const idle=pinned&&!own.length;fill(x,top,lw,H,r,cols,idle?.12:.22);fill(x,h-bot-bh,lw,bh,[r,r,0,0],cols,idle?.4:1)}
  /* each run of threads with the same owners: a rule in their colours, under their names when the names fit */
  ctx.textAlign='center';ctx.textBaseline='alphabetic';ctx.fillStyle=text2;
  for(const run of runsOf(owners.map(o=>o.map(g=>g.id).join('+')))){const own=owners[run.from];if(!own.length)continue;
    const x0=run.from*(lw+gap),x1=run.to*(lw+gap)+lw;own.forEach((g,k)=>{ctx.fillStyle=guestColour(g);ctx.fillRect(x0+k*(x1-x0)/own.length,top-6,(x1-x0)/own.length,2)});
    ctx.fillStyle=text2;const names=own.map(guestName).join(' + '),room=x1-x0;const label=ctx.measureText(names).width<=room?names:own.length>1?(ctx.measureText('Shared').width<=room?'Shared':''):clip(names,room);
    if(label)ctx.fillText(label,(x0+x1)/2,top-11)}
  /* each run of one kind of thread, named in full when that fits and by its letter when it does not */
  if(kinds){ctx.fillStyle=text2;ctx.textBaseline='bottom';
    for(const run of runsOf(kinds.map(k=>k||''))){if(!run.key)continue;const x0=run.from*(lw+gap),x1=run.to*(lw+gap)+lw;
      const label=[KIND_WORD[run.key],KIND_SHORT[run.key]].find(t=>ctx.measureText(t).width<=x1-x0);if(label)ctx.fillText(label,(x0+x1)/2,h)}}
  ctx.textAlign='left'}
function wireCores(c){if(!c)return;const wrap=c.parentElement;const tip=$('.tip',wrap);let hideT=0;
  const show=e=>{const u=T.cores;if(!u||!u.length)return;const n=u.length;const r=c.getBoundingClientRect();const i=clamp(Math.floor((e.clientX-r.left)/r.width*n),0,n-1);
    const own=coreOwners()[i]||[],kind=T.coreKinds&&T.coreKinds[i];
    tip.textContent=['Thread '+i,kind?KIND_WORD[kind]:'',own.map(guestName).join(' + '),Math.round(u[i]*100)+'%'].filter(Boolean).join(' · ');
    tip.hidden=false;const tw=tip.offsetWidth||150;tip.style.left=clamp((i+.5)/n*r.width,tw/2+2,Math.max(tw/2+2,r.width-tw/2-2))+'px'};
  c.addEventListener('pointerdown',e=>{clearTimeout(hideT);show(e)});c.addEventListener('pointermove',e=>{if(e.pointerType==='mouse'||e.buttons)show(e)});
  c.addEventListener('pointerleave',e=>{if(e.pointerType==='mouse')tip.hidden=true});c.addEventListener('pointerup',e=>{if(e.pointerType!=='mouse'){clearTimeout(hideT);hideT=setTimeout(()=>{tip.hidden=true},2200)}})}
/* smooth filled charts: a bezier through every point, a soft fill that fades to the sheet, and a marker you can touch */
/* a readable top of scale: the smallest "nice" number above the data's peak */
function niceCeil(v){if(v<=0)return 1;const p=Math.pow(10,Math.floor(Math.log10(v)));for(const m of [1,1.5,2,2.5,3,4,5,6,8,10]){if(m*p>=v)return m*p}return 10*p}
function drawArea(c,arr,o){
  if(!c)return;const {ctx,w,h}=fitCanvas(c,136);ctx.clearRect(0,0,w,h);const n=arr.length;const arr2=o.arr2||null;
  if(n<2){ctx.font='15px '+cssv('--font');ctx.fillStyle=cssv('--text-2');ctx.textAlign='center';ctx.textBaseline='middle';ctx.fillText(o.empty||NR,w/2,h/2);ctx.textAlign='left';return}
  const min=o.min;const max=o.fit?niceCeil(Math.max(1,...arr,...(arr2||[]))*1.08):o.max;
  const axis=!!o.range;const padT=(o.limit!=null||o.fit)?22:10,padB=axis?22:8;
  const y=v=>h-padB-clamp((v-min)/(max-min),0,1)*(h-padT-padB);const x=i=>n>1?i/(n-1)*w:0;
  const col=o.color||cssv('--tint-soft');const font=cssv('--font');
  const path=a=>{ctx.beginPath();ctx.moveTo(x(0),y(a[0]));for(let i=1;i<n;i++){const px=x(i-1),py=y(a[i-1]),cx=x(i),cy=y(a[i]);const mx=(px+cx)/2;ctx.bezierCurveTo(mx,py,mx,cy,cx,cy)}};
  if(n>1){ctx.save();path(arr);ctx.lineTo(w,h-padB);ctx.lineTo(0,h-padB);ctx.closePath();ctx.clip();ctx.globalAlpha=.42;ctx.fillStyle=col;ctx.fillRect(0,0,w,h);ctx.globalAlpha=1;ctx.globalCompositeOperation='destination-out';const g=ctx.createLinearGradient(0,padT,0,h-padB);g.addColorStop(0,cssv('--fade-0'));g.addColorStop(1,cssv('--fade-1'));ctx.fillStyle=g;ctx.fillRect(0,0,w,h);ctx.restore()}
  ctx.strokeStyle=cssv('--line');ctx.lineWidth=1;ctx.beginPath();ctx.moveTo(0,y(min)+.5);ctx.lineTo(w,y(min)+.5);ctx.stroke();
  ctx.font='13px '+font;
  if(o.fit){ctx.fillStyle=cssv('--text-2');ctx.textBaseline='top';ctx.textAlign='left';ctx.fillText(o.fmt(max),2,2)}
  if(o.limit!=null){ctx.strokeStyle=cssv('--red');ctx.setLineDash([4,4]);ctx.beginPath();ctx.moveTo(0,y(o.limit)+.5);ctx.lineTo(w,y(o.limit)+.5);ctx.stroke();ctx.setLineDash([]);if(o.limitLabel){ctx.fillStyle=cssv('--red');ctx.textBaseline='bottom';ctx.textAlign='right';ctx.fillText(o.limitLabel,w-4,y(o.limit)-3)}}
  /* the time axis: where the range starts, each midnight, and now */
  if(axis&&n>1){ctx.fillStyle=cssv('--text-2');ctx.textBaseline='bottom';const ticks=axisTicks(o.times||[],o.range);let lastX=-1e9;
    for(const t of ticks){const tx=x(t.i);if(t.mid){ctx.strokeStyle=cssv('--line');ctx.beginPath();ctx.moveTo(Math.round(tx)+.5,padT-4);ctx.lineTo(Math.round(tx)+.5,h-padB);ctx.stroke()}
      ctx.textAlign=t.i===0?'left':t.i===n-1?'right':'center';const tw=ctx.measureText(t.l).width;const x0=t.i===0?tx:t.i===n-1?tx-tw:tx-tw/2;if(x0<lastX+10)continue;ctx.fillText(t.l,t.i===0?tx+2:t.i===n-1?tx-2:tx,h-3);lastX=x0+tw}}
  if(n>1&&arr2){path(arr2);ctx.strokeStyle=cssv('--text-2');ctx.lineWidth=1.5;ctx.lineJoin='round';ctx.stroke()}
  if(n>1){path(arr);ctx.strokeStyle=col;ctx.lineWidth=2;ctx.lineJoin='round';ctx.stroke()}
  if(o.marker!=null&&n>1){const i=clamp(o.marker,0,n-1);ctx.strokeStyle=cssv('--text-3');ctx.lineWidth=1;ctx.beginPath();ctx.moveTo(Math.round(x(i))+.5,padT-6);ctx.lineTo(Math.round(x(i))+.5,h-padB);ctx.stroke();
    const dot=(v,cl)=>{ctx.fillStyle=cssv('--glass-solid');ctx.beginPath();ctx.arc(x(i),y(v),6.5,0,7);ctx.fill();ctx.fillStyle=cl;ctx.beginPath();ctx.arc(x(i),y(v),4.5,0,7);ctx.fill()};
    if(arr2)dot(arr2[i],cssv('--text-2'));dot(arr[i],col)}
  ctx.textAlign='left';
}
function chartHtml(id,label){return `<div class="chartwrap"><canvas class="chart" id="${id}" role="img" tabindex="0" aria-label="${esc(label)}. Touch it, or use the arrow keys, to read one value."></canvas><div class="tip num" hidden></div></div>`}
/* every readout names its day, so "what happened yesterday" has an answer */
function dayWord(t){const today=new Date();if(t.toDateString()===today.toDateString())return 'Today';const y=new Date(today);y.setDate(today.getDate()-1);if(t.toDateString()===y.toDateString())return 'Yesterday';return DAYS[t.getDay()]}
function timeAt(ms,range){if(ms==null)return '';const t=new Date(ms);return range==='1h'?hhmm(t):dayWord(t)+' '+hhmm(t)}
function axisTicks(times,range){const n=times.length;if(n<2)return [];const ticks=[];const at=i=>new Date(times[i]);
  if(range==='1h')ticks.push({i:0,l:hhmm(at(0))});
  else{for(let i=1;i<n-1;i++){const t=at(i),prev=at(i-1);if(t.getDate()!==prev.getDate())ticks.push({i,l:range==='24h'?'Midnight':DAYS[t.getDay()].slice(0,3),mid:true})}
    if(range==='24h')ticks.unshift({i:0,l:dayWord(at(0))+' '+hhmm(at(0))})}
  ticks.push({i:n-1,l:'Latest'});return ticks}
function wireChart(c,get){
  if(!c)return;const wrap=c.parentElement;const tip=$('.tip',wrap);wrap._idx=null;
  const paint=()=>{const {arr,o}=get();drawArea(c,arr,Object.assign({},o,{marker:wrap._idx}));if(wrap._idx==null||!arr.length){tip.hidden=true;c.removeAttribute('aria-valuetext')}else{const i=clamp(wrap._idx,0,arr.length-1);const txt=(o.arr2&&o.fmt2?o.fmt2(arr[i],o.arr2[i]):o.fmt(arr[i]))+' · '+timeAt((o.times||[])[i],o.range);tip.hidden=false;tip.textContent=txt;c.setAttribute('aria-valuetext',txt);const r=c.getBoundingClientRect();const tw=tip.offsetWidth||112;tip.style.left=clamp(i/Math.max(1,arr.length-1)*r.width,tw/2+2,Math.max(tw/2+2,r.width-tw/2-2))+'px'}};
  const at=e=>{const r=c.getBoundingClientRect();const {arr}=get();wrap._idx=clamp(Math.round((e.clientX-r.left)/r.width*(arr.length-1)),0,arr.length-1);paint()};
  let down=false,hideT=0;
  c.addEventListener('pointerdown',e=>{down=true;clearTimeout(hideT);try{c.setPointerCapture(e.pointerId)}catch(err){}at(e)});
  c.addEventListener('pointermove',e=>{if(down||e.pointerType==='mouse')at(e)});
  const up=e=>{down=false;if(e.pointerType!=='mouse'){clearTimeout(hideT);hideT=setTimeout(()=>{wrap._idx=null;paint()},2200)}};
  c.addEventListener('pointerup',up);c.addEventListener('pointercancel',up);
  c.addEventListener('pointerleave',e=>{if(e.pointerType==='mouse'&&!down){wrap._idx=null;paint()}});
  c.addEventListener('keydown',e=>{const {arr}=get();if(e.key==='ArrowLeft'||e.key==='ArrowRight'){e.preventDefault();wrap._idx=clamp((wrap._idx==null?arr.length-1:wrap._idx)+(e.key==='ArrowLeft'?-1:1),0,arr.length-1);paint()}else if(e.key==='Escape'){wrap._idx=null;paint()}});
  c.addEventListener('blur',()=>{wrap._idx=null;paint()});
  wrap._paint=paint;paint();
}
function paintCharts(root){$$('.chartwrap',root).forEach(w=>w._paint&&w._paint())}
/* open a page at a section: the sheet body scrolls so that heading sits at the top */
function jumpTo(root,sel){if(!sel)return;const h=$(sel,root);if(!h)return;const go=()=>{const sb=$('#sbody');if(h.isConnected)sb.scrollTop=Math.max(0,h.offsetTop-sb.offsetTop-10)};go();setTimeout(go,0)}
function rangeHtml(){return `<div class="segc rangec" role="group" aria-label="Time range">${Object.entries(RANGES).map(([k,R])=>`<button data-range="${k}" aria-pressed="${RANGE===k}">${R.label}</button>`).join('')}</div>`}
function wireRange(root){$$('[data-range]',root).forEach(b=>b.onclick=()=>{RANGE=b.dataset.range;store.set('range',RANGE);$$('[data-range]',root).forEach(x=>x.setAttribute('aria-pressed',String(x.dataset.range===RANGE)));$$('.chartwrap',root).forEach(w=>{w._idx=null;w._paint&&w._paint()})})}
/* health: plain counts, never a reassuring zero from a source that has gone quiet */
function healthHtml(key){
  if(key!=='host')return `<h3 class="roomhead" style="font-size:17px" id="hl-${esc(key)}">Health, last 24 hours</h3>${notReportingBand('Health readings are only collected for '+T.hostName+'.')}`;
  const H=HEALTHDATA;const counters=faultSection();const rep=!!counters;
  const known=new Set(FAULTS.flatMap(f=>f[3]));
  const extra=rep?Object.keys(counters.counters||{}).filter(k=>!known.has(k)&&counters.counters[k].status==='ok'):[];
  const disks=H&&H.disks&&H.disks.status==='ok'?H.disks.disks:[];
  const pools=H&&H.storage&&H.storage.status==='ok'?H.storage.pools:[];
  const rd=c=>c&&c.status==='ok'?c.value:'—';
  return `<h3 class="roomhead" style="font-size:17px" id="hl-${key}">Health, last 24 hours</h3>
    ${rep?'':notReportingBand('The collector is not reporting, so nothing below is current.')}
    <div class="counts">${FAULTS.map(([code,label,tech])=>{const n=faultValue(code);const ok=n!=null;return `<button data-open="faults:${key}:${code}" class="${!ok?'na':n?'bad':''}" aria-label="${label}: ${ok?n:'not reporting'}. Open the entries."><b class="num">${ok?n:'—'}</b><small>${label}</small><small>${ok?tech:NR}</small></button>`}).join('')}${extra.map(k=>`<button class="${counters.counters[k].value?'bad':''}" aria-label="${esc(k)}: ${esc(counters.counters[k].value)}"><b class="num">${esc(counters.counters[k].value)}</b><small>${esc(k.replace(/_/g,' '))}</small><small>from the collector</small></button>`).join('')}</div>
    <div class="rows">${disks.length?disks.map(d=>`<div class="rowi">${ICON('nas')}<div class="t"><b>${esc(d.name||'Disk')}</b><small>Disk health ${d.health.status==='ok'?`<span class="${/pass|ok/i.test(String(d.health.value))?'okword':'badword'}">${esc(d.health.value)}</span>`:NR} · ${d.media_errors.status==='ok'?d.media_errors.value+' media error'+(d.media_errors.value===1?'':'s'):'media errors not reporting'}</small></div></div>`).join(''):row('nas','Disks',NR,null)}${pools.map(p=>`<div class="rowi">${ICON('nas')}<div class="t"><b>${esc(p.name||'Pool')}</b><small>Read ${esc(rd(p.read_errors))} · write ${esc(rd(p.write_errors))} · checksum ${esc(rd(p.checksum_errors))} · scrub ${esc(rd(p.scrub_errors))} errors</small></div></div>`).join('')}</div>`;
}
const monitorSheets={
  faults(arg){
    const [key,code]=arg.split(':');const f=FAULTS.find(x=>x[0]===code)||FAULTS[0];const name=machineName(key);const n=faultValue(f[0]);
    const html=`<p class="p2">${esc(f[1])} on ${esc(name)}, last 24 hours. <span class="fine">${esc(f[2])}, from the kernel log</span></p>
      ${n==null?notReportingBand(key==='host'?'The collector did not report this count.':'Health readings are only collected for '+T.hostName+'.'):`<p class="sent">${n} in the last 24 hours.</p><p class="p2">The collector gives a count, not the individual entries.</p>`}`;
    return {title:f[1],html};
  },
};

export {chartGet,lastOf,cssv,chartHtml,wireChart,paintCharts,jumpTo,rangeHtml,wireRange,healthHtml,coreSummary,coreLegendInner,guestName,drawCores,wireCores,monitorSheets};
