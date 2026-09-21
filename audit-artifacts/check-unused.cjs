// Conservative audit: literal property reads and dynamic dictionary subtrees.
// Run after npm ci. Results are candidates for review, not automatic deletions.
const fs=require('fs'), path=require('path');
const base=path.resolve(__dirname,'../crates/pc-api/web');
const ts=require(path.join(base,'node_modules/typescript'));
const parse=p=>ts.createSourceFile(p,fs.readFileSync(p,'utf8'),ts.ScriptTarget.Latest,true,p.endsWith('tsx')?ts.ScriptKind.TSX:ts.ScriptKind.TS);
const defs=[]; const source=parse(path.join(base,'src/locales/ru.ts'));
function object(node,prefix){
  if(ts.isAsExpression(node))return object(node.expression,prefix);
  if(!ts.isObjectLiteralExpression(node))return;
  for(const p of node.properties){if(!ts.isPropertyAssignment(p))continue;
    const key=prefix+'.'+p.name.text;
    if(ts.isObjectLiteralExpression(p.initializer))object(p.initializer,key);
    else defs.push({key,line:source.getLineAndCharacterOfPosition(p.getStart()).line+1});
  }
}
function definition(n){if(ts.isVariableDeclaration(n)&&['ui','messages'].includes(n.name.text))object(n.initializer,n.name.text);ts.forEachChild(n,definition)}definition(source);
const used=new Set(), dynamic=new Set(), files=fs.readdirSync(path.join(base,'src')).filter(p=>/\.tsx?$/.test(p));
function chain(n){if(ts.isIdentifier(n))return n.text;if(ts.isPropertyAccessExpression(n))return chain(n.expression)+'.'+n.name.text;if(ts.isElementAccessExpression(n)&&ts.isStringLiteral(n.argumentExpression))return chain(n.expression)+'.'+n.argumentExpression.text;return '';}
for(const f of files){const ast=parse(path.join(base,'src',f));function visit(n){
  if(ts.isPropertyAccessExpression(n))used.add(chain(n));
  if(ts.isElementAccessExpression(n)){if(ts.isStringLiteral(n.argumentExpression))used.add(chain(n));else dynamic.add(chain(n.expression));}
  if(ts.isCallExpression(n)&&n.expression.getText(ast)==='t'&&n.arguments.length&&ts.isStringLiteral(n.arguments[0]))used.add('messages.'+n.arguments[0].text);
  ts.forEachChild(n,visit);
}visit(ast)}
// i18n.ts uses active.ui.title for document.title.
used.add('ui.title');
const unused=defs.filter(({key})=>!used.has(key)&&![...dynamic].some(p=>p&&key.startsWith(p+'.')));
const css=fs.readFileSync(path.join(base,'src/style.css'),'utf8');
const code=files.map(f=>fs.readFileSync(path.join(base,'src',f),'utf8')).join('\n');
const classes=new Map();
for(const m of css.matchAll(/\.([a-zA-Z_][\w-]*)/g)){if(!classes.has(m[1]))classes.set(m[1],css.slice(0,m.index).split('\n').length);}
const cssCandidates=[...classes].filter(([c])=>!code.includes(c)).map(([name,line])=>({name,line}));
console.log(JSON.stringify({method:'AST locale accesses; preserve dynamically indexed subtrees. CSS class names absent as text from all TS/TSX; manual review required.',localeLeaves:defs.length,dynamicSubtrees:[...dynamic].filter(p=>p.startsWith('ui.')),unusedLocaleCandidates:unused,cssClassCount:classes.size,unusedCssCandidates:cssCandidates},null,2));
