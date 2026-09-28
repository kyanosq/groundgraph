import com.sun.source.tree.*;
import com.sun.source.util.*;
import javax.lang.model.element.*;
import javax.lang.model.type.*;
import javax.tools.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;

/** JDK 11+ compiler adapter: parse + attribute only; runs annotation processors only when configured. */
class GroundGraphJava {
    final Path root;
    final PrintWriter output;
    final Trees trees;
    final JavacTask task;
    final Map<CompilationUnitTree, int[]> bytes = new IdentityHashMap<>();
    final List<TreePath> calls = new ArrayList<>(), methods = new ArrayList<>();
    final List<Diagnostic<? extends JavaFileObject>> errors = new ArrayList<>();
    final Map<String,List<long[]>> errorsByFile = new HashMap<>();
    final Map<JavaFileObject,String> uris = new IdentityHashMap<>();
    final Map<CompilationUnitTree,String> pathsByUnit = new IdentityHashMap<>();
    final Map<Element,TreePath> declarationPaths = new IdentityHashMap<>();
    CompilationUnitTree annotationUnit;

    GroundGraphJava(Path root, PrintWriter out, JavacTask task) {
        this.root = root; this.output = out; this.task = task; this.trees = Trees.instance(task);
    }
    static Map<String,Object> obj(Object... pairs) {
        Map<String,Object> out = new LinkedHashMap<>();
        for (int i=0;i<pairs.length;i+=2) out.put((String)pairs[i],pairs[i+1]);
        return out;
    }
    // Small output-only JSON encoder; no dependency or permissive JSON parser.
    static String json(Object value) {
        if (value == null) return "null";
        if (value instanceof Number || value instanceof Boolean) return value.toString();
        if (value instanceof Map) {
            List<String> fields = new ArrayList<>();
            ((Map<?,?>)value).forEach((k,v)->fields.add(json(k.toString())+":"+json(v)));
            return "{"+String.join(",",fields)+"}";
        }
        if (value instanceof List) {
            List<String> items = new ArrayList<>();
            for (Object v: (List<?>)value) items.add(json(v));
            return "["+String.join(",",items)+"]";
        }
        StringBuilder s = new StringBuilder("\"");
        for (char c: value.toString().toCharArray()) {
            switch(c) {
                case '"': s.append("\\\""); break;
                case '\\': s.append("\\\\"); break;
                case '\n': s.append("\\n"); break;
                case '\r': s.append("\\r"); break;
                case '\t': s.append("\\t"); break;
                default: if (c < 32) s.append(String.format("\\u%04x",(int)c)); else s.append(c);
            }
        }
        return s.append('"').toString();
    }
    void emit(Map<String,Object> row) { output.println(json(row)); }
    String uri(JavaFileObject file) { return uris.computeIfAbsent(file,f->f.toUri().toString()); }
    String path(CompilationUnitTree unit) { return pathsByUnit.computeIfAbsent(unit,u->root.relativize(Paths.get(u.getSourceFile().toUri())).toString().replace(File.separatorChar,'/')); }
    long start(TreePath p) { return trees.getSourcePositions().getStartPosition(p.getCompilationUnit(),p.getLeaf()); }
    int byteAt(CompilationUnitTree unit, long position) {
        if (position < 0) return -1;
        int[] offsets = bytes.get(unit);
        return position < offsets.length ? offsets[(int)position] : -1;
    }
    Map<String,Object> location(TreePath p, String prefix) {
        return obj(prefix+"path",path(p.getCompilationUnit()),prefix+"start",byteAt(p.getCompilationUnit(),start(p)));
    }
    TreePath declaration(Element element) {
        if(!declarationPaths.containsKey(element)) declarationPaths.put(element,trees.getPath(element));
        return declarationPaths.get(element);
    }
    void collect(CompilationUnitTree unit) throws IOException {
        String source = unit.getSourceFile().getCharContent(true).toString();
        int[] offsets = new int[source.length()+1]; int count=0;
        for(int i=0;i<source.length();) {
            int cp=source.codePointAt(i), width=Character.charCount(cp);
            offsets[i]=count;
            if(width==2) offsets[i+1]=count;
            count += cp<=0x7f ? 1 : cp<=0x7ff ? 2 : cp<=0xffff ? 3 : 4;
            i+=width; offsets[i]=count;
        }
        bytes.put(unit,offsets);
        new TreePathScanner<Void,Void>() {
            public Void visitMethod(MethodTree t, Void p) { methods.add(getCurrentPath()); return super.visitMethod(t,p); }
            public Void visitMethodInvocation(MethodInvocationTree t, Void p) { calls.add(getCurrentPath()); return super.visitMethodInvocation(t,p); }
            public Void visitNewClass(NewClassTree t, Void p) { calls.add(getCurrentPath()); return super.visitNewClass(t,p); }
            public Void visitMemberReference(MemberReferenceTree t, Void p) { calls.add(getCurrentPath()); return super.visitMemberReference(t,p); }
        }.scan(unit,null);
    }
    boolean erroneous(TypeMirror t) {
        return erroneous(t,Collections.newSetFromMap(new IdentityHashMap<>()),0);
    }
    boolean erroneous(TypeMirror t,Set<TypeMirror> visited,int depth) {
        if(t==null || t.getKind()==TypeKind.ERROR) return true;
        if(!visited.add(t)) return false;
        if(depth>32) return true;
        if(t.getKind()==TypeKind.ARRAY) return erroneous(((ArrayType)t).getComponentType(),visited,depth+1);
        if(t.getKind()==TypeKind.DECLARED) {
            for(TypeMirror a:((DeclaredType)t).getTypeArguments()) if(erroneous(a,visited,depth+1)) return true;
        }
        if(t.getKind()==TypeKind.TYPEVAR) return erroneous(((TypeVariable)t).getUpperBound(),visited,depth+1);
        if(t.getKind()==TypeKind.WILDCARD) {
            WildcardType w=(WildcardType)t;
            return (w.getExtendsBound()!=null && erroneous(w.getExtendsBound(),visited,depth+1)) || (w.getSuperBound()!=null && erroneous(w.getSuperBound(),visited,depth+1));
        }
        return false;
    }
    boolean overlapsError(TreePath p) {
        long a=start(p), b=trees.getSourcePositions().getEndPosition(p.getCompilationUnit(),p.getLeaf());
        for(long[] range: errorsByFile.getOrDefault(uri(p.getCompilationUnit().getSourceFile()),Collections.emptyList())) {
            long x=range[0], y=range[1];
            if(x>=0 && x<b && (y<0 ? x : Math.max(x,y))>=a) return true;
        }
        return false;
    }
    void binding(TreePath p, boolean completed) {
        Map<String,Object> row = location(p,""); row.put("kind","call"); row.put("resolved",false); row.put("reason","unresolved_symbol");
        row.put("end",byteAt(p.getCompilationUnit(),trees.getSourcePositions().getEndPosition(p.getCompilationUnit(),p.getLeaf())));
        try {
            Element e=trees.getElement(p);
            if(completed && !overlapsError(p) && e instanceof ExecutableElement) {
                ExecutableElement m=(ExecutableElement)e;
                boolean bad=erroneous(m.getEnclosingElement().asType()) || erroneous(m.getReturnType());
                if(p.getLeaf() instanceof MethodInvocationTree) {
                    ExpressionTree select=((MethodInvocationTree)p.getLeaf()).getMethodSelect();
                    if(select instanceof MemberSelectTree) {
                        TreePath receiver=new TreePath(new TreePath(p,select),((MemberSelectTree)select).getExpression());
                        bad |= receiver==null || erroneous(trees.getTypeMirror(receiver));
                    }
                }
                for(VariableElement v:m.getParameters()) bad |= erroneous(v.asType());
                if(!bad) {
                    row.put("resolved",true); row.put("reason","compiler_static_binding");
                    row.put("symbol",m.getEnclosingElement()+"."+m);
                    List<Object> args=ownerTypeArgs(p,m);
                    if(args!=null) row.put("owner_type_args",args);
                    TreePath target=declaration(m);
                    if(target!=null && bytes.containsKey(target.getCompilationUnit()) && start(target)>=0) row.putAll(location(target,"target_"));
                } else row.put("reason","error_type_in_binding");
            } else if(!completed) row.put("reason","compiler_analysis_failed");
            else if(overlapsError(p)) row.put("reason","compiler_error_at_call");
        } catch(RuntimeException ex) { row.put("reason","binding_exception:"+ex.getClass().getSimpleName()); }
        emit(row);
    }
    /** 继承来的泛型方法（如 BaseMapper<T>.insert）：接收者在声明类型上的实参，带源码路径，供框架约定落到实体。 */
    List<Object> ownerTypeArgs(TreePath p, ExecutableElement m) {
        Element owner=m.getEnclosingElement();
        if(!(owner instanceof TypeElement) || ((TypeElement)owner).getTypeParameters().isEmpty() || m.getModifiers().contains(Modifier.STATIC)) return null;
        TypeMirror receiver=null;
        ExpressionTree select=p.getLeaf() instanceof MethodInvocationTree ? ((MethodInvocationTree)p.getLeaf()).getMethodSelect() : null;
        if(select instanceof MemberSelectTree) {
            ExpressionTree r=((MemberSelectTree)select).getExpression();
            if(!(r instanceof IdentifierTree && ((IdentifierTree)r).getName().contentEquals("super"))) receiver=trees.getTypeMirror(new TreePath(new TreePath(p,select),r));
        }
        if(receiver==null) {
            TypeElement enclosing=trees.getScope(p).getEnclosingClass();
            if(enclosing!=null) receiver=enclosing.asType();
        }
        javax.lang.model.util.Types types=task.getTypes();
        Deque<TypeMirror> queue=new ArrayDeque<>();
        Set<String> seen=new HashSet<>();
        if(receiver!=null) queue.add(receiver);
        while(!queue.isEmpty()) {
            TypeMirror t=queue.poll();
            if(!(t instanceof DeclaredType) || !seen.add(t.toString())) continue;
            DeclaredType d=(DeclaredType)t;
            if(d.asElement().equals(owner)) {
                List<Object> out=new ArrayList<>();
                for(TypeMirror a:d.getTypeArguments()) {
                    TypeMirror e=types.erasure(a);
                    Element el=types.asElement(e);
                    TreePath decl=el==null?null:trees.getPath(el);
                    out.add(obj("name",e.toString(),"path",decl!=null && bytes.containsKey(decl.getCompilationUnit())?path(decl.getCompilationUnit()):null));
                }
                return out;
            }
            queue.addAll(types.directSupertypes(t));
        }
        return null;
    }
    void overrides() {
        Map<String,List<ExecutableElement>> byName=new HashMap<>();
        for(TreePath p:methods) {
            Element e=trees.getElement(p);
            if(e instanceof ExecutableElement && e.getKind()==ElementKind.METHOD)
                byName.computeIfAbsent(e.getSimpleName().toString(), k->new ArrayList<>()).add((ExecutableElement)e);
        }
        for(List<ExecutableElement> group:byName.values()) for(ExecutableElement impl:group) {
            if(impl.getModifiers().contains(Modifier.ABSTRACT) || !(impl.getEnclosingElement() instanceof TypeElement)) continue;
            for(ExecutableElement base:group) if(base!=impl) {
                try {
                    if(task.getElements().overrides(impl,base,(TypeElement)impl.getEnclosingElement())) {
                        TreePath ip=declaration(impl), bp=declaration(base);
                        if(ip!=null && bp!=null) {
                            Map<String,Object> row=obj("kind","override"); row.putAll(location(ip,"target_")); row.putAll(location(bp,"base_")); emit(row);
                        }
                    }
                } catch(RuntimeException ex) { emit(obj("kind","diagnostic","message","override_binding_exception:"+ex.getClass().getSimpleName())); }
            }
        }
    }
    static String head(AnnotationTree a) { String s=a.getAnnotationType().toString(); return s.substring(s.lastIndexOf('.')+1); }
    List<String> values(ExpressionTree e) {
        List<String> out=new ArrayList<>();
        if(e instanceof LiteralTree && ((LiteralTree)e).getValue() instanceof String) out.add(((LiteralTree)e).getValue().toString());
        else if(e instanceof NewArrayTree && ((NewArrayTree)e).getInitializers()!=null)
            for(ExpressionTree v:((NewArrayTree)e).getInitializers()) out.addAll(values(v));
        else if(e instanceof BinaryTree && e.getKind()==Tree.Kind.PLUS)
            out.add(first(values(((BinaryTree)e).getLeftOperand()))+first(values(((BinaryTree)e).getRightOperand())));
        else {
            Object constant=null;
            TreePath p=TreePath.getPath(annotationUnit,e);
            if(p!=null) {
                Element element=trees.getElement(p);
                if(element instanceof VariableElement) constant=((VariableElement)element).getConstantValue();
            }
            out.add(constant instanceof String ? constant.toString() : "<unresolved:"+e+">");
        }
        return out;
    }
    List<String> attr(AnnotationTree a, String key) {
        for(ExpressionTree arg:a.getArguments()) {
            if(arg instanceof AssignmentTree) {
                AssignmentTree x=(AssignmentTree)arg;
                if(x.getVariable().toString().equals(key)) {
                    if(key.equals("method")) {
                        List<? extends ExpressionTree> exprs=x.getExpression() instanceof NewArrayTree ? ((NewArrayTree)x.getExpression()).getInitializers() : Collections.singletonList(x.getExpression());
                        List<String> verbs=new ArrayList<>();
                        if(exprs!=null) for(ExpressionTree v:exprs) {
                            String name=v instanceof MemberSelectTree?((MemberSelectTree)v).getIdentifier().toString():v.toString();
                            if(Arrays.asList("GET","POST","PUT","DELETE","PATCH","HEAD","OPTIONS","TRACE").contains(name)) verbs.add(name);
                            else verbs.add("<unresolved:"+v+">");
                        }
                        return verbs;
                    }
                    return values(x.getExpression());
                }
            } else if(key.equals("value")) return values(arg);
        }
        return Collections.emptyList();
    }
    static String first(List<String> xs) { return xs.isEmpty()?"":xs.get(0); }
    List<String> paths(AnnotationTree a) {
        List<String> p=attr(a,"path"); if(p.isEmpty()) p=attr(a,"value");
        return p.isEmpty()?Collections.singletonList(""):p;
    }
    static AnnotationTree find(List<? extends AnnotationTree> as, String name) {
        for(AnnotationTree a:as) if(head(a).equals(name)) return a;
        return null;
    }
    static String route(String a,String b) {
        String path=("/"+a+"/"+b).replaceAll("/+","/");
        if(path.length()>1 && path.endsWith("/")) path=path.substring(0,path.length()-1);
        return path.replaceAll("\\{[^}]+\\}","{}");
    }
    String annotationName(TreePath method, AnnotationTree a) {
        TreePath modifiers=new TreePath(method,((MethodTree)method.getLeaf()).getModifiers());
        Element e=trees.getElement(new TreePath(new TreePath(modifiers,a),a.getAnnotationType()));
        if(e instanceof TypeElement && !erroneous(e.asType())) return ((TypeElement)e).getQualifiedName().toString();
        String name=a.getAnnotationType().toString();
        if(name.contains(".")) return name;
        // Missing jars: explicit imports still document intent. Wildcard imports
        // are not enough to distinguish a same-package or shadowed annotation.
        for(ImportTree i:annotationUnit.getImports()) {
            String imported=i.getQualifiedIdentifier().toString();
            if(!i.isStatic() && imported.endsWith("."+name)) return imported;
        }
        return null;
    }
    void frameworkAnnotations() {
        for(TreePath p:methods) {
            annotationUnit=p.getCompilationUnit();
            for(AnnotationTree a:((MethodTree)p.getLeaf()).getModifiers().getAnnotations()) {
                String shortName=head(a);
                if(!Arrays.asList("Select","Insert","Update","Delete","SelectProvider","InsertProvider","UpdateProvider","DeleteProvider","Scheduled","EventListener","TransactionalEventListener").contains(shortName)) continue;
                String name=annotationName(p,a);
                boolean unknown=name==null;
                boolean sql=(unknown || name.equals("org.apache.ibatis.annotations."+shortName)) &&
                    Arrays.asList("Select","Insert","Update","Delete","SelectProvider","InsertProvider","UpdateProvider","DeleteProvider").contains(shortName);
                boolean entry=Arrays.asList("org.springframework.scheduling.annotation.Scheduled", "org.springframework.context.event.EventListener", "org.springframework.transaction.event.TransactionalEventListener").contains(name) ||
                    (unknown && Arrays.asList("Scheduled","EventListener","TransactionalEventListener").contains(shortName));
                if(!sql && !entry) continue;
                long pos=trees.getSourcePositions().getStartPosition(annotationUnit,a);
                Map<String,Object> row=location(p,"");
                row.putAll(obj("kind","framework","role",sql?"sql":"entrypoint","annotation",unknown?a.getAnnotationType().toString():name,
                    "annotation_start",byteAt(annotationUnit,pos),"line",annotationUnit.getLineMap().getLineNumber(pos),
                    "resolution","candidate","reason","source_annotation_runtime_binding_unverified"));
                if(unknown) { row.put("resolution","unresolved"); row.put("reason","annotation_type_unresolved"); }
                if(sql) {
                    String text=String.join(" ",attr(a,"value"));
                    row.put("stmt_kind",shortName.replace("Provider","").toLowerCase(Locale.ROOT));
                    row.put("sql",text);
                    String reason=shortName.endsWith("Provider")?"sql_provider_not_evaluated":
                        text.isEmpty() || text.contains("<unresolved:")?"sql_constant_unresolved":
                        text.contains("${")?"dynamic_sql_substitution":null;
                    if(reason!=null) { row.put("resolution","unresolved"); row.put("reason",reason); }
                }
                emit(row);
            }
        }
    }
    void routes() {
        for(TreePath p:methods) {
            annotationUnit=p.getCompilationUnit();
            TreePath parent=p.getParentPath();
            while(parent!=null && !(parent.getLeaf() instanceof ClassTree)) parent=parent.getParentPath();
            if(parent==null) continue;
            ClassTree owner=(ClassTree)parent.getLeaf();
            List<? extends AnnotationTree> ca=owner.getModifiers().getAnnotations();
            AnnotationTree feign=find(ca,"FeignClient");
            boolean server=find(ca,"RestController")!=null || find(ca,"Controller")!=null;
            if(feign==null && !server) continue;
            String service=feign==null?"":first(attr(feign,"name"));
            if(feign!=null && service.isEmpty()) service=first(attr(feign,"value"));
            AnnotationTree cm=find(ca,"RequestMapping");
            List<String> prefixes=cm==null?Collections.singletonList(""):paths(cm);
            String clientPrefix=feign==null?"":first(attr(feign,"path"));
            for(AnnotationTree a:((MethodTree)p.getLeaf()).getModifiers().getAnnotations()) {
                String name=head(a), verb="";
                switch(name) {
                    case "GetMapping": verb="GET"; break; case "PostMapping": verb="POST"; break;
                    case "PutMapping": verb="PUT"; break; case "DeleteMapping": verb="DELETE"; break;
                    case "PatchMapping": verb="PATCH"; break; case "RequestMapping": verb="ANY"; break;
                    default: continue;
                }
                List<String> verbs=name.equals("RequestMapping")?attr(a,"method"):Collections.singletonList(verb);
                if(verbs.isEmpty()) verbs=Collections.singletonList("ANY");
                for(String prefix:prefixes) for(String sub:paths(a)) for(String v:verbs) {
                    if((service+prefix+sub+clientPrefix+v).contains("<unresolved:") || (service+prefix+sub+clientPrefix).contains("${") || (feign!=null && !attr(feign,"url").isEmpty() && !first(attr(feign,"url")).isEmpty())) {
                        emit(obj("kind","diagnostic","path",path(p.getCompilationUnit()),"message","unresolved_feign_or_route_configuration at line "+p.getCompilationUnit().getLineMap().getLineNumber(start(p))));
                        continue;
                    }
                    Map<String,Object> row=location(p,""); row.put("kind","route"); row.put("role",feign==null?"server":"client");
                    row.put("service",service); row.put("route",route(clientPrefix+"/"+prefix,sub)); row.put("verb",v);
                    row.put("line",p.getCompilationUnit().getLineMap().getLineNumber(start(p))); emit(row);
                }
            }
        }
    }
    public static void main(String[] args) throws Exception {
        Path root=Paths.get(args[0]).toRealPath();
        JavaCompiler compiler=ToolProvider.getSystemJavaCompiler();
        if(compiler==null) throw new IllegalStateException("JDK compiler is required");
        DiagnosticCollector<JavaFileObject> diagnostics=new DiagnosticCollector<>();
        try(StandardJavaFileManager manager=compiler.getStandardFileManager(diagnostics,Locale.ROOT,StandardCharsets.UTF_8);
            PrintWriter out=new PrintWriter(Files.newBufferedWriter(Paths.get(args[2]),StandardCharsets.UTF_8))) {
            List<File> sources=new ArrayList<>();
            for(String path:Files.readAllLines(Paths.get(args[1]),StandardCharsets.UTF_8)) sources.add(root.resolve(path).toFile());
            // Implicit source discovery stays disabled; annotation processors run only when
            // explicitly configured (args[4]), with generated output confined to args[5].
            List<String> options=new ArrayList<>(Arrays.asList("-implicit:none","-Xlint:none","-encoding","UTF-8","-classpath",args[3],"-sourcepath","","-Xmaxerrs","1000000"));
            if(args[4].isEmpty()) options.add("-proc:none");
            else options.addAll(Arrays.asList("-processorpath",args[4],"-s",args[5],"-d",args[5]));
            JavacTask task=(JavacTask)compiler.getTask(null,manager,diagnostics,options,null,manager.getJavaFileObjectsFromFiles(sources));
            GroundGraphJava g=new GroundGraphJava(root,out,task);
            for(CompilationUnitTree unit:task.parse()) g.collect(unit);
            boolean completed=true;
            try { task.analyze(); }
            catch(RuntimeException ex) { completed=false; g.emit(obj("kind","diagnostic","message","analysis_exception:"+ex.getClass().getSimpleName())); }
            for(Diagnostic<? extends JavaFileObject> d:diagnostics.getDiagnostics()) if(d.getKind()==Diagnostic.Kind.ERROR) {
                g.errors.add(d);
                if(d.getSource()!=null) g.errorsByFile.computeIfAbsent(g.uri(d.getSource()), k->new ArrayList<>()).add(new long[]{d.getStartPosition(),d.getEndPosition()});
                String path=d.getSource()==null?null:root.relativize(Paths.get(d.getSource().toUri())).toString().replace(File.separatorChar,'/');
                g.emit(obj("kind","diagnostic","path",path,"message",d.getCode()+" at line "+d.getLineNumber()));
            }
            g.routes();
            g.frameworkAnnotations();
            for(TreePath p:g.methods) { Element e=g.trees.getElement(p); if(e!=null) g.declarationPaths.put(e,p); }
            for(TreePath p:g.calls) g.binding(p,completed);
            if(completed) g.overrides();
            g.emit(obj("kind","summary","calls",g.calls.size(),"errors",g.errors.size(),"analysis_completed",completed));
            if(out.checkError()) throw new IOException("cannot write compiler output");
        }
    }
}
