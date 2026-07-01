import type { B, I, M, Union } from 'ts-toolbelt';

declare const __kind__: unique symbol;

declare const __capture__: unique symbol;

type True = 1;

type Primitive = boolean | string | number | bigint | symbol | undefined | null;

type ExtractSubcapture<T> =
    T extends M.Primitive | M.BuiltIn
        ? never
        : T extends object
            ? T[Exclude<keyof T, keyof [] | keyof {}>]
            : never;

type PartialAssignment<K, V> = V extends never ? never : K extends string ? { [k in K]: V } : never;

type EmptyToNever<T> = {} extends T ? never : T;

export type Kind = 'any' | 'rest' | 'string' | 'number' | 'boolean' | 'function' | 'symbol' | 'bigint' | 'object';

export interface Hole<Type = any, Label = any> {
    T: Type;
    readonly [__kind__]: Label;
};

interface __any extends Hole<any, 'any'> {};

export interface __string extends Hole<string, 'string'> {};

export interface __number extends Hole<number, 'number'> {};

export interface __boolean extends Hole<boolean, 'boolean'> {};

export interface __symbol extends Hole<symbol, 'symbol'> {};

export interface __bigint extends Hole<bigint, 'bigint'> {};

export interface __object extends Hole<Object, 'object'> {};

export interface __function extends Hole<Function, 'function'> {};

export interface __rest extends Hole<any[], 'rest'> {};

export interface __ extends Hole<any, 'any'> {
    string: __string;
    number: __number;
    boolean: __boolean;
    object: __object;
    function: __function;
    symbol: __symbol;
    bigint: __bigint;
    any: __any;
    rest: __rest;
    tail: __rest;
    tl: __rest;
};

export type HoleInnerType<T> = T extends Hole<infer U> ? U : T;

export interface Capture<Name extends String = any, Pattern = any> {
    readonly [__capture__]: Name;
    readonly pattern: Pattern;
};

type CapturePattern<T> = T extends Capture<any, infer Pattern> ? Pattern : T;

type RecursiveExpandCapture<T> =
    T extends any
        ? any extends T
            ? T
            : T extends Hole | Capture
                ? ExpandCapture<T>
                : T extends Record<string, any>
                    ? { [K in keyof T]: ExpandCapture<T[K]> }
                    : never
        : T extends Hole | Capture
            ? ExpandCapture<T>
            : T extends Record<string, any>
                ? { [K in keyof T]: ExpandCapture<T[K]> }
                : never;

export type ExpandCapture<T> = RecursiveExpandCapture<CapturePattern<HoleInnerType<T>>>;

type NeverToEmpty<T> = [T] extends [never] ? {} : T;

type Explode<T> = T[keyof T];

export type RecursiveCollect<Node> = NeverToEmpty<DoRecursiveCollect<Node>>;

type IsLeafNode<T> =
    T extends Record<string, any>
        ? T extends Hole
            ? true
            : T extends any
                ? any extends T
                    ? true
                    : false
                : false
        : true;

type WalkChildren<Node> = Explode<{ [Key in keyof Node]: DoRecursiveCollect<Node[Key]> }>;

type DoRecursiveCollect<Node> =
    IsLeafNode<Node> extends True
        ? never
        : Node extends Capture<any, infer P>
            ? Node | DoRecursiveCollect<P>
            : WalkChildren<Node>;

type UnionizeCapture<T> = T extends Capture<infer K, infer V> ? K extends string ? { [k in K]: V } : never : never;

export type CaptureToEntry<T> = Union.Merge<UnionizeCapture<T>>;

export type VariableCapture<T> = ExpandCapture<CaptureToEntry<RecursiveCollect<T>>>;

export type PatternHandler<Pattern> = (arg0: VariableCapture<Pattern>) => unknown;

type IsTopType<T> =
    T extends unknown
        ? unknown extends T
            ? true
            : T extends object
                ? object extends T
                    ? true
                    : T extends Object
                        ? Object extends T
                            ? true
                            : T extends any
                                ? any extends T
                                    ? true
                                    : never
                                : never
                        : T extends any
                            ? any extends T
                                ? true
                                : never
                            : never
                : T extends Object
                    ? Object extends T
                        ? true
                        : T extends any
                            ? any extends T
                                ? true
                                : never
                            : never
                    : T extends any
                        ? any extends T
                            ? true
                            : never
                        : never
        : T extends object
            ? object extends T
                ? true
                : T extends Object
                    ? Object extends T
                        ? true
                        : T extends any
                            ? any extends T
                                ? true
                                : never
                            : never
                    : T extends any
                        ? any extends T
                            ? true
                            : never
                        : never
            : T extends Object
                ? Object extends T
                    ? true
                    : T extends any
                        ? any extends T
                            ? true
                            : never
                        : never
                : T extends any
                    ? any extends T
                        ? true
                        : never
                    : never;

export type UnifyAll<Value, Pattern> =
    IsTopType<Value> extends True
        ? VariableCapture<Pattern>
        : Overlapping<Value, ExpandCapture<Pattern>> extends {}
            ? DoUnifyAll<Value, Pattern>
            : never;

export type DoUnifyAll<Value, Pattern> =
    Pattern extends Capture<infer VarName, infer Child>
        ? PartialAssignment<VarName, Overlapping<Value, ExpandCapture<Child>>> | UnifyAll<Value, Child>
        : Overlapping<Value, ExpandCapture<Pattern>> extends object
            ? ExtractSubcapture<{
                [K in keyof Value]: K extends keyof Pattern ? UnifyAll<Value[K], Pattern[K]> : never
            }>
            : never;

export type ShouldUnifyAll<Value, Pattern> =
    Pattern extends any
        ? any extends Pattern
            ? Value extends any
                ? any extends Value
                    ? false
                    : Pattern extends Primitive
                        ? Value extends Primitive
                            ? false
                            : true
                        : true
                : Pattern extends Primitive
                    ? Value extends Primitive
                        ? false
                        : true
                    : true
            : Pattern extends Primitive
                ? Value extends Primitive
                    ? false
                    : true
                : true
        : Pattern extends Primitive
            ? Value extends Primitive
                ? false
                : true
            : true;

export type CaseParameters<Value, Pattern> = EmptyToNever<Union.Merge<UnifyAll<Value, Pattern>>>;

export type Overlapping<A, B, Depth extends I.Iteration = I.IterationOf<0>> =
    A.Equals<A, any> extends True
        ? B
        : A extends B
            ? A
            : [A, B, keyof A] extends [any[], any[], any] | [object, object, keyof B]
                ? RejectMismatch<{
                    [Key in keyof A]: Key extends keyof B ? Overlapping<A[Key], B[Key], I.Next<Depth>> : A[Key]
                }>
                : never;

export type RejectMismatch<T> = {} extends T ? never : T extends AnyNever<A.Cast<T, object>> ? never : T;

export type AnyNever<T extends object> =
    keyof T extends infer S
        ? S extends any
            ? { [K in A.Cast<S, string | number | symbol>]: never }
            : never
        : never;


