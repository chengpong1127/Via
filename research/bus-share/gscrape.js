// Paste into the built-in browser's javascript tool on a Google Maps transit directions page (hl=zh-TW), then call:
//   (await window.gscrape()).join('\n')
// URL for a departure time: https://www.google.com/maps/dir/<lat,lon>/<lat,lon>/data=!4m6!4m5!2m3!6e0!7e2!8j<EPOCH>!3e3?hl=zh-TW
// where EPOCH is the local wall-clock time encoded as if it were UTC (08:00 Taipei on 2026-11-27 = 1795766400).
// It clicks every option, opens its details, and prints one compact line per option (W = total walk minutes, B[] bus leg, M[] rail/metro leg).
window.gparse=function(t){
 const k=t.split(' | ').map(x=>x.replace(/[​-‏  ⁠　﻿-]/g,' ').trim()).filter(x=>x.length>0);
 const isT=x=>/^(上午|下午)\d+:\d+$/.test(x);
 const hdr=(k.find(x=>/\(\d+ 分\)/.test(x))||'').replace(/\(星期.\)\s*/,'').replace(/ - .*\(/,' ('); let walk=0; const legs=[];
 for(let i=0;i<k.length;i++){
  if(k[i]==='步行' && /^約 \d+ 分/.test(k[i+1]||'')) walk+=+k[i+1].match(/約 (\d+) 分/)[1];
  else if(/^\d+ 分$/.test(k[i]) && /^\(\d+ 站\)$/.test(k[i+1]||'')){
    let j=i+2; while(j<k.length && !isT(k[j])) j++;
    const line=k[i-1]; const rail=/(Line|Rail|Train|Express|Limited|機場線|區間|自強|莒光|普悠瑪|太魯閣)/.test(line);
    legs.push((rail?'M[':'B[')+line+'] '+k[i-2]+'>'+k[j+1]+' '+k[i].replace(' 分','m')+(isT(k[i-3])?' @'+k[i-3]:''));
  }
 }
 return hdr+' W'+walk+' ; '+legs.join(' ; ');
};
window.gscrape = async function(){
 const sleep=ms=>new Promise(r=>setTimeout(r,ms));
 const waitList=async()=>{for(let q=0;q<20;q++){ if(document.querySelector('[data-trip-index]')) return true; await sleep(300);} return false;};
 await waitList(); await sleep(1500);
 const n=new Set([...document.querySelectorAll('[data-trip-index]')].map(e=>e.dataset.tripIndex)).size;
 const out=[];
 for(let i=0;i<n;i++){
  if(!document.querySelector('[data-trip-index="'+i+'"]')){await waitList(); }
  const e2=document.querySelector('[data-trip-index="'+i+'"]'); if(!e2) {out.push('(missing '+i+')'); continue;}
  e2.click(); await sleep(1200);
  let b=[...document.querySelectorAll('button')].find(b=>b.innerText.includes('詳細資料')); if(b){b.click(); await sleep(1800);}
  const m=document.querySelector('[role="main"]')||document.body;
  let t=m.innerText.replace(/\n+/g,' | ').replace(/\t+/g,'').replace(/( \| )+/g,' | ');
  out.push(window.gparse(t));
  const back=[...document.querySelectorAll('button')].find(b=>b.getAttribute('aria-label')==='返回');
  if(back){back.click(); await sleep(1200);}
  if(!document.querySelector('[data-trip-index]')){ const back2=[...document.querySelectorAll('button')].find(b=>b.getAttribute('aria-label')==='返回'); if(back2){back2.click(); await sleep(1200);} await waitList(); }
 }
 return out;
};
