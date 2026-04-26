package com.example.demo;

import java.util.List;
import java.util.ArrayList;

public class Sample {

    private String name;
    private int count = 0;

    public Sample(String name) {
        this.name = name;
        this.count = 1;
    }

    public void simpleMethod() {
        String local = "hello";
        System.out.println(local);
        return;
    }

    public int compute(int x, int y) {
        int result = x + y;
        if (result > 100) {
            System.out.println("big");
            return 100;
        }
        for (int i = 0; i < result; i++) {
            count += i;
        }
        return result;
    }

    public void iterateList(List<String> items) {
        for (String item : items) {
            System.out.println(item);
        }
        int total = items.size();
        System.out.println(total);
    }

    public void tryCatchExample() {
        try {
            int val = Integer.parseInt("42");
            System.out.println(val);
        } catch (NumberFormatException e) {
            System.err.println(e.getMessage());
        }
    }

    public void lambdaExample(List<String> data) {
        data.forEach(s -> System.out.println(s));
        List<String> filtered = new ArrayList<>();
        data.stream()
            .filter(item -> item.length() > 3)
            .forEach(item -> filtered.add(item));
    }

    // Inner class
    public static class Inner {
        public void innerMethod() {
            int x = 10;
            System.out.println(x);
        }
    }
}
