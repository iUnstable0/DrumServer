package org.example.drumcontroller;

import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;
import org.springframework.boot.context.properties.ConfigurationPropertiesScan;


@SpringBootApplication
@ConfigurationPropertiesScan
public class DrumControllerApplication {

    public static void main(String[] args) {
        SpringApplication.run(DrumControllerApplication.class, args);
    }

}