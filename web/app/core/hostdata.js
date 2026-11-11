/* ============================================================ what a telemetry snapshot says about threads, guests and energy.
   Pure functions: no page, no request. Nothing here knows how many threads a machine has, which computers run on it or what
   they are called. Every number comes from the snapshot, and one that is missing, null or not a number stays missing. */

const MAX_GUESTS=64,MAX_CPUS=4096,MAX_DOMAINS=8;
/* each guest keeps one colour from this many, by its rank among the guests the collector lists; with more guests than colours they repeat, and the names say which is which */
const PALETTE=8;
const num=v=>typeof v==='number'&&isFinite(v)?v:null;
const text=(v,max)=>typeof v==='string'?v.trim().slice(0,max):'';

/* a CPU list as the collector sends it: whole numbers, each once, in order. Anything else in it is left out */
function cpuSet(list){
  if(!Array.isArray(list))return null;
  const seen=new Set();
  for(const x of list){if(Number.isInteger(x)&&x>=0&&x<MAX_CPUS)seen.add(x)}
  return [...seen].sort((a,b)=>a-b);
}
/* [0,1,2,3,8,9] as "0-3, 8-9" */
function cpuText(list){
  const out=[];
  for(let i=0;i<list.length;){let j=i;while(j+1<list.length&&list[j+1]===list[j]+1)j++;out.push(j>i?list[i]+'-'+list[j]:String(list[i]));i=j+1}
  return out.join(', ');
}

/* the guests of the machine, by id. `affinity` is the threads it is pinned to, null when it may run on any thread, and left
   undefined when the collector could not read its configuration, which says nothing about where it runs */
function guestsOf(snapshot){
  const raw=snapshot&&Array.isArray(snapshot.guests)?snapshot.guests:[];
  const seen=new Set(),out=[];
  for(const g of raw.slice(0,MAX_GUESTS)){
    if(!g||typeof g!=='object')continue;
    const id=typeof g.id==='string'&&/^\d{1,9}$/.test(g.id)?Number(g.id):num(g.id);
    if(id==null||!Number.isInteger(id)||id<0||seen.has(id))continue;
    seen.add(id);
    let affinity;
    if(g.cpu_affinity===null)affinity=null;
    else if(g.cpu_affinity!==undefined){const set=cpuSet(g.cpu_affinity);affinity=set&&set.length?set:null}
    out.push({id,name:text(g.name,64),kind:text(g.type,16),status:text(g.status,16),running:g.status==='running',
      memUsed:num(g.mem_used_bytes),memHost:num(g.mem_host_bytes),memAssigned:num(g.mem_assigned_bytes)!=null?num(g.mem_assigned_bytes):num(g.mem_total_bytes),
      cpuCount:num(g.cpu_count),affinity,slot:0});
  }
  out.sort((a,b)=>a.id-b.id);
  out.forEach((g,i)=>{g.slot=i%PALETTE+1});
  return out;
}

/* what each thread is: 'performance', 'efficiency' or null, when the collector says and for as many threads as it reports load for */
function kindsOf(snapshot,n){
  const k=snapshot&&snapshot.host&&snapshot.host.cpu_core_kinds;
  if(!Array.isArray(k)||k.length!==n)return null;
  const kinds=k.map(x=>x==='performance'||x==='efficiency'?x:null);
  return kinds.some(x=>x)?kinds:null;
}

/* the guests pinned to each thread: a list per thread, empty when none is, longer than one when threads are shared */
function ownersByThread(n,guests){
  const owners=Array.from({length:n},()=>[]);
  for(const g of guests)if(g.affinity)for(const cpu of g.affinity)if(cpu<n)owners[cpu].push(g);
  return owners;
}

/* runs of neighbouring threads that share one key: [{from,to,key}] */
function runsOf(keys){
  const runs=[];
  for(let i=0;i<keys.length;){let j=i;while(j+1<keys.length&&keys[j+1]===keys[i])j++;runs.push({from:i,to:j,key:keys[i]});i=j+1}
  return runs;
}

/* the energy counter, as the collector keeps it. `since` becomes milliseconds like every time the page handles */
function energyOf(snapshot){
  const e=snapshot&&snapshot.energy;
  if(!e||typeof e!=='object')return null;
  const kwh=num(e.total_kwh);
  if(kwh==null||kwh<0)return null;
  const domains=(Array.isArray(e.domains)?e.domains:[]).slice(0,MAX_DOMAINS).filter(d=>d&&typeof d==='object').map(d=>({
    id:text(d.id,64),label:text(d.label,64)||text(d.id,64),kwh:num(d.kwh),watts:num(d.watts),source:text(d.source,16)}));
  const watts=num(snapshot.energy_watts)!=null?num(snapshot.energy_watts):domains.some(d=>d.watts!=null)?domains.reduce((s,d)=>s+(d.watts||0),0):null;
  return {kwh,since:num(e.since)!=null?e.since*1000:null,scope:text(e.scope,16),watts,domains};
}

/* the host's memory split by who holds it: each guest's share of the host's RAM, the host's own use, and what is free.
   A guest's share is the host memory it takes (`mem_host_bytes`), else what it says it uses. The host's own use is what is
   used beyond the guests. Missing totals give null and the page draws nothing, never a guess */
function memoryParts(total,hostUsed,guests){
  if(num(total)==null||total<=0)return null;
  const parts=[];let held=0;
  for(const g of guests){const bytes=g.memHost!=null?g.memHost:g.memUsed;if(bytes!=null&&bytes>0){parts.push({guest:g,bytes});held+=bytes}}
  if(!parts.length)return null;
  const host=num(hostUsed)!=null?Math.max(0,hostUsed-held):null;
  const used=host!=null?held+host:held;
  return {total,parts,held,host,free:host!=null?Math.max(0,total-used):null,scale:Math.max(total,used)};
}

const GIB=1073741824;
const gbText=b=>b==null?'—':b<GIB/10?Math.round(b/1048576)+' MB':(b/GIB).toFixed(b<10*GIB?1:0)+' GB';
const kwhText=k=>k==null?'—':k.toFixed(k<10?3:k<1000?2:1)+' kWh';
/* money is shown with its currency code and two decimals; it is an estimate, so it is rounded here and nowhere else */
const moneyText=(x,currency)=>x==null?'—':x.toFixed(2)+' '+currency;
/* a price as a person types it: a decimal point or comma. Null when it is not a price the hub accepts */
function priceFrom(textValue){
  const x=Number(String(textValue).trim().replace(',','.'));
  return Number.isFinite(x)&&x>0&&x<=1000000?x:null;
}
const currencyFrom=t=>/^[A-Za-z]{3}$/.test(String(t).trim())?String(t).trim().toUpperCase():null;

export {PALETTE,cpuSet,cpuText,guestsOf,kindsOf,ownersByThread,runsOf,energyOf,memoryParts,gbText,kwhText,moneyText,priceFrom,currencyFrom};
