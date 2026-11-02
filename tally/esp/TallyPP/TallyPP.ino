/*
 * WebSocketClient.ino
 *
 *  Created on: 24.05.2015
 *
 */

#include <Arduino.h>
#include <EEPROM.h>

#include <ESP8266WiFi.h>
//#include <ArduinoJson.h>

#include <WebSocketsClient.h>

#include <Hash.h>

#include <Adafruit_NeoPixel.h>
#ifdef __AVR__
 #include <avr/power.h> // Required for 16 MHz Adafruit Trinket
#endif

// define the number of bytes you want to access
#define EEPROM_SIZE 1

#define SSID ""
#define PASS ""

#define HOST "tally.lan" //brdige
#define PORT 9240 // Enter server port

#define USE_SERIAL Serial1

#define SW_1 16
#define SW_2 14
#define SW_3 12
#define SW_4 13

#define LED_PIN 0 //14;
#define NUMPIXELS 7 // Popular NeoPixel ring size

//ESP8266WiFiMulti WiFiMulti;
WebSocketsClient webSocket;

Adafruit_NeoPixel pixels(NUMPIXELS, LED_PIN, NEO_GRB + NEO_KHZ800);

bool tally_locked = false;

int luminance = 255; //at full

uint32_t black = pixels.Color(0, 0, 0);
uint32_t gray = pixels.Color(100, 80, 60);
uint32_t red = pixels.Color(255, 0, 0);
uint32_t green = pixels.Color(0, 255, 0);
uint32_t blue = pixels.Color(0, 0, 255);
uint32_t orange = pixels.Color(255, 100, 0);
uint32_t white = pixels.Color(255, 255, 255);
uint32_t violet = pixels.Color(255, 0, 250);
uint32_t pink = pixels.Color(255,190,200);
uint32_t yellow = pixels.Color(255, 200, 0);

int tally_id = 0;

bool io_status = false;
bool wifi_status = false;
bool atem_status = true; //not handled for now

int pgm = 0;
int pvw = 0;
int trs = 0;

uint32_t signal_color = black;
uint32_t tally_color = gray;

//
unsigned long interval=100;
// Tracks the time since last event fired
unsigned long previousMillis=0;
//
bool signal_state = false;
//

String hostname = "ESP Tally";

void doSignal(uint32_t color, int start_led = 0, int end_led = NUMPIXELS, float koef = 1){
  if( color == signal_color ){
    return;
  }
  USE_SERIAL.println("SIGNAL");
  //fill strip with same color from start to end, others pins stay untouched
  pixels.fill(black, 0, NUMPIXELS); //reset tp full black
  pixels.fill(color, start_led, end_led);
  //set luminance of whole strip // WARNING CAN BE LOSS by using lower value without reseting the color to the full bright
  pixels.setBrightness(luminance);
  //
  int lumi = round(luminance * koef);
  if( lumi < 50 ){
    lumi = 50;
  }else if( lumi > 255 ){
    lumi = 255;
  }
  //
  pixels.setBrightness(lumi);

  //send defined pattern to the led strip
  pixels.show();
  signal_color = color; //set last
}

void webSocketEvent(WStype_t type, uint8_t * payload, size_t length) {

    switch(type) {
    case WStype_ERROR:
      USE_SERIAL.printf("[WSc] Error!!!\n");
      break;
        case WStype_DISCONNECTED:
            USE_SERIAL.printf("[WSc] Disconnected!\n");
      io_status = false;
            break;
        case WStype_CONNECTED: {
            USE_SERIAL.printf("[WSc] Connected to url: %s\n", payload);
      io_status = true;
      doSignal(white,0,1);
            // send message to server when Connected
      String message = "JOIN " + hostname;
            webSocket.sendTXT(message);
        }
            break;
        case WStype_TEXT:
            USE_SERIAL.printf("[WSc] get text: %s\n", payload);
      //String message = (char*)payload;
      parseMessage(payload);
            break;
        case WStype_BIN:
            USE_SERIAL.printf("[WSc] get binary length: %u\n", length);
            //hexdump(payload, length);
            break;
    case WStype_PING:
      // pong will be send automatically
      //USE_SERIAL.printf("[WSc] get ping\n");
      break;
    case WStype_PONG:
      // answer to a ping we send
      //USE_SERIAL.printf("[WSc] get pong\n");
      break;
    default:
      USE_SERIAL.printf("[WSc] Default event\n");
      break;
    }

}

String converter(uint8_t *str){
    String message = String((char *)str);
    message.trim();
    return message;
}

void parseMessage( uint8_t * payload ){

  String message = converter(payload);

  int cmd_delimiter_index = message.indexOf(' ');
  //USE_SERIAL.println(cmd_delimiter_index);

  String cmd = "Test";

  if( cmd_delimiter_index > 1){
    cmd = message.substring(0,cmd_delimiter_index);
  }else{
    cmd = message;
  }

  //USE_SERIAL.println(cmd);
    if(cmd.equals("TALLY-LOCK")){
      String id = message.substring(cmd.length());
      if(id.toInt() == 1){
        tally_locked = true;
        tally_color = yellow;
      }else{
        tally_locked = false;
        tally_color = gray;
      }
    }else if(cmd.equals("TALLY-PGM") || cmd.equals("TALLY-PVW")){
    String id = message.substring(cmd.length());
    //id = id.substring(0,id.indexOf(' '));
    USE_SERIAL.println(id);
    if(cmd.equals("TALLY-PGM")){
      trs = pgm;
      pgm = id.toInt();
    }
    if(cmd.equals("TALLY-PVW")){
      pvw = id.toInt();
    }

    if( pgm == tally_id ){
      tally_color = red;
    }else if( pvw == tally_id ){
      tally_color = green;
    }else{
      tally_color = gray;
    }

  }else if(cmd.equals("TALLY-TRS")){
    if( pgm == tally_id || pvw == tally_id || trs == tally_id){
      tally_color = red;
    }
  }else if(cmd.equals("SET-LUMINANCE")){
    String data = message.substring(cmd.length());
    luminance = data.toInt();
      if( luminance < 50 ){
        luminance = 50;
      }else if( luminance > 255 ){
        luminance = 255;
      }
      USE_SERIAL.println(luminance);
      pixels.setBrightness(luminance);
      pixels.show(); // light up
      EEPROM.write(0, luminance);
      EEPROM.commit();
  }

}

void setup() {
  pixels.begin(); // INITIALIZE NeoPixel pixel object (REQUIRED)

    USE_SERIAL.begin(115200); delay(500); //Waiting for serial

  // initialize EEPROM with predefined size
  EEPROM.begin(EEPROM_SIZE);

  // read the last LED state from flash memory
  if( EEPROM.read(0) > 5 ){
    luminance = EEPROM.read(0);
  }

  doSignal(white,0,1); //power on

    USE_SERIAL.setDebugOutput(true);

    USE_SERIAL.println();
    USE_SERIAL.println();
    USE_SERIAL.println();

  pinMode(LED_BUILTIN, OUTPUT);
  digitalWrite(LED_BUILTIN, HIGH); //Powerpack keepalive internal led //this is not needed, becayse i dont use whole off (my black is single gray)

  pinMode(SW_1, INPUT_PULLUP); //NO INPUT_PULLUP on wemos D1 //PULLED hardway by 10k resistor
  pinMode(SW_2, INPUT_PULLUP);
  pinMode(SW_3, INPUT_PULLUP);
  pinMode(SW_4, INPUT_PULLUP);


  delay(1000); //human reaction to power on

  if( digitalRead(SW_1) == LOW ){
      tally_id += 1;
  }

  if( digitalRead(SW_2) == LOW ){
      tally_id += 2;
  }

  if( digitalRead(SW_3) == LOW ){
      tally_id += 3;
  }

  if( digitalRead(SW_4) == LOW ){
      tally_id += 4;
  }

  for (int i = 0; i < tally_id; ++i)
  {
    doSignal(red,0,1);
    delay(100);
    doSignal(black);
    delay(400);
  }

    USE_SERIAL.print( "TALLY ID: " ); USE_SERIAL.println( tally_id );
    //strcpy(watch_id, id.c_str());

    WiFi.mode(WIFI_STA); //SET STATION MODE // wifi simple client to avoid switching between modes

    //Get Default Hostname
    USE_SERIAL.printf("Default hostname: %s\n", WiFi.hostname().c_str());

    //Set new hostname
    hostname = WiFi.hostname() + "-" + tally_id;

    //String hostname = WiFi.hostname() + "-" + tally_id;
    WiFi.hostname(hostname);

    //Get Current Hostname
    USE_SERIAL.printf("New hostname: %s\n", WiFi.hostname().c_str());

    WiFi.begin(SSID, PASS);

    //WiFi.disconnect();
    while(WiFi.status() != WL_CONNECTED) {
    doSignal(blue,0,1);
    delay(100);
    doSignal(black);
    delay(400);
    }

    // server address, port and URL
    webSocket.begin(HOST, PORT, "/");

    // event handler
    webSocket.onEvent(webSocketEvent);

    // use HTTP Basic Authorization this is optional remove if not needed
    //webSocket.setAuthorization("user", "Password");

    // try ever 5000 again if connection has failed
    webSocket.setReconnectInterval(5000);

  // start heartbeat (optional)
  // ping server every 15000 ms
  // expect pong from server within 3000 ms
  // consider connection disconnected if pong is not received 2 times
  webSocket.enableHeartbeat(15000, 3000, 2);

}

void loop() {

  unsigned long currentMillis = millis();

    if(WiFi.status() == WL_CONNECTED) {
    webSocket.loop();
    wifi_status = true;
  } else {
    webSocket.disconnect();
    wifi_status = false;
  }

  if( wifi_status == false || io_status == false || atem_status == false ){
      uint32_t color = black;
      if( wifi_status == false ){
        color = blue;
      }else if( io_status == false ){
        color = orange;
      }else if( atem_status == false ){
        color = violet;
      }

       // How much time has passed, accounting for rollover with subtraction!
       if ((unsigned long)(currentMillis - previousMillis) >= interval) {
          // It's time to do something!
          //USE_SERIAL.println("Signal");
      if(signal_state){
        interval = 100;
          doSignal(black);
      }else{
        interval = 900;
          doSignal(color,0,1); //on
      }
          signal_state = !signal_state;

          // Use the snapshot to set track time until next event
          previousMillis = currentMillis;
       }

       //


    }else{
      // Get snapshot of time
      interval = 0;
      if( tally_color == gray ){
        interval = 50;
      }

      if(tally_locked){
        tally_color = yellow;
      }
       // How much time has passed, accounting for rollover with subtraction!
       if ((unsigned long)(currentMillis - previousMillis) >= interval) {
        USE_SERIAL.println("SIGNALING...");
          if( tally_color == gray ){
            doSignal(tally_color,0,1); //o
          }else if( tally_color == green ){
            doSignal(tally_color,1); //o
          }else{
            doSignal(tally_color); //o
          }
          previousMillis = currentMillis;
       }
    }
}
